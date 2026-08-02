//! Translate a tmux-style key-spec into routed input events (phux-21o,
//! ADR-0022, phux-3j3).
//!
//! Each CLI argument is either a **named key** (`Enter`, `Tab`, `Escape`,
//! `Up`, `C-c`, `M-x`, …) or a **literal string**. Literal strings normally
//! type character by character, matching `tmux send-keys`. A literal run
//! immediately before `Enter` is instead a submission-safe paste followed by
//! the real key, so a paste-aware TUI can distinguish text from submission.
//! Named-key bytes and ordinary literal bytes feed the *same* [`StdinParser`]
//! the interactive client uses for real keystrokes; no hand-rolled char→key
//! table is involved.
//!
//! The built events ride the side-effect-free `ROUTE_INPUT` control
//! command (L1.md §5.1) rather than an `ATTACH` + `INPUT_KEY` stream. So,
//! like `GET_SCREEN`, this neither subscribes the caller nor resizes the
//! pane to the caller's viewport — the live session keeps its dimensions
//! (phux-3j3).

use std::path::Path;

use phux_protocol::TerminalId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, Command, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::attach::input::StdinParser;
use crate::state::report_degradation;

/// Translate one key-spec argument to the bytes a terminal would receive.
///
/// Named keys map to their control/escape bytes; `C-<x>` is a control
/// byte, `M-<x>` is ESC-prefixed; anything else is literal UTF-8 (sent
/// char by char by the parser downstream).
#[must_use]
pub fn spec_to_bytes(arg: &str) -> Vec<u8> {
    match arg.to_ascii_lowercase().as_str() {
        "enter" | "return" => return b"\r".to_vec(),
        "tab" => return b"\t".to_vec(),
        "escape" | "esc" => return vec![0x1b],
        "space" => return b" ".to_vec(),
        "bspace" | "backspace" => return vec![0x7f],
        "up" => return b"\x1b[A".to_vec(),
        "down" => return b"\x1b[B".to_vec(),
        "right" => return b"\x1b[C".to_vec(),
        "left" => return b"\x1b[D".to_vec(),
        "home" => return b"\x1b[H".to_vec(),
        "end" => return b"\x1b[F".to_vec(),
        _ => {}
    }
    // `C-x` → control byte (C-a = 0x01 … C-z = 0x1a; C-[ = ESC, etc.).
    if let Some(rest) = strip_prefix_ci(arg, "c-")
        && rest.chars().count() == 1
        && let Some(c) = rest.chars().next()
        && c.is_ascii()
    {
        return vec![(c.to_ascii_uppercase() as u8) & 0x1f];
    }
    // `M-x` → ESC-prefixed (Alt).
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

/// Translate all key-spec args into structured [`InputEvent`]s.
///
/// A contiguous literal run immediately followed by `Enter`/`Return` is one
/// trusted paste event followed by the real Enter key. The server then honors
/// the pane's live DEC mode 2004 state: paste-aware TUIs receive explicit
/// bracketed-paste delimiters before Enter, so they cannot mistake a fast text
/// burst plus carriage return for a multiline paste. Literal input elsewhere
/// retains tmux-shaped character-by-character semantics.
///
/// The events are routed by id via `ROUTE_INPUT`; they are not addressed to a
/// Terminal here.
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
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "enter"
            | "return"
            | "tab"
            | "escape"
            | "esc"
            | "space"
            | "bspace"
            | "backspace"
            | "up"
            | "down"
            | "right"
            | "left"
            | "home"
            | "end"
    ) || strip_prefix_ci(arg, "c-").is_some_and(|rest| rest.chars().count() == 1)
        || strip_prefix_ci(arg, "m-").is_some()
}

/// Resolve `target` to the Terminal id of its focused pane, against a
/// `GET_STATE` snapshot. Mirrors the server's own focus rule: a session's
/// `active_window`, then that window's `active_pane`. For
/// [`AttachTarget::Last`] (no name) we defer to the snapshot's
/// server-wide `focused_pane`.
fn resolve_focused_pane(snapshot: &SessionSnapshot, target: &AttachTarget) -> Option<TerminalId> {
    let session = match target {
        AttachTarget::Last => return Some(snapshot.focused_pane.clone()),
        AttachTarget::ByName(name) | AttachTarget::CreateIfMissing { name, .. } => {
            snapshot.sessions.iter().find(|s| &s.name == name)?
        }
        AttachTarget::ById(id) => snapshot.sessions.iter().find(|s| s.id == *id)?,
        // `AttachTarget` is `#[non_exhaustive]`; a future target a newer
        // build introduces is not resolvable here.
        _ => return None,
    };
    let active_window = session.active_window?;
    snapshot
        .windows
        .iter()
        .find(|w| w.id == active_window)
        .and_then(|w| w.active_pane.clone())
}

/// Send `keys` to the focused pane of `target` via the side-effect-free
/// `ROUTE_INPUT` route, returning the resolved pane id.
///
/// No `ATTACH`: the target's focused pane is resolved client-side from a
/// `GET_STATE` snapshot (the same way `phux snapshot`/`kill` resolve
/// selectors), then each built [`InputEvent`] is delivered with a
/// `ROUTE_INPUT` command. Because nothing attaches and no viewport is
/// advertised, the live pane keeps its dimensions — unlike the old
/// attach-then-`INPUT_KEY` path, which transiently resized the pane to the
/// caller's `80x24` viewport (phux-3j3).
///
/// This is the focused-pane convenience over [`send_to`]: callers that have
/// already resolved a selector to a concrete pane (e.g. via the CLI's full
/// `TARGET` grammar, phux-n95) call [`send_to`] directly and skip this
/// extra `GET_STATE` round trip.
///
/// Returns the [`TerminalId`] of the focused pane the keys were sent to,
/// so callers (e.g. `phux run`) can read back the *same* pane.
pub async fn send(
    socket: &Path,
    target: AttachTarget,
    keys: &[String],
) -> Result<TerminalId, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    // Resolve the focused pane without attaching (side-effect-free).
    //
    // GET_STATE on a hub interleaves one uncorrelated ERROR per unreachable
    // satellite ahead of the merged snapshot, so this cannot use
    // `into_result_ignoring_interleaved`. A degraded snapshot here is not
    // fatal — the focused pane we are looking for is usually local — but it
    // changes what "no such session" means, so it is reported rather than
    // dropped (`report_degradation` logs; the CLI prints).
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
    let pane = resolve_focused_pane(&snapshot, &target).ok_or_else(|| {
        AttachError::Refused("no such session, or it has no focused pane".to_owned())
    })?;
    route_keys(&mut conn, &pane, keys).await?;
    Ok(pane)
}

/// Send `keys` to a pre-resolved `pane` via the side-effect-free
/// `ROUTE_INPUT` route.
///
/// The pane-targeted core of [`send`]: the caller has already resolved a
/// selector (the CLI's full `TARGET` grammar — `session`, `session:window`,
/// `session:window.pane`, `@id`; phux-n95) to a concrete [`TerminalId`], so
/// there is no `GET_STATE` round trip and no focus heuristic — the keys land
/// on exactly the pane named. Like [`send`], nothing attaches, so the live
/// pane keeps its dimensions (phux-3j3).
///
/// # Errors
///
/// Propagates [`AttachError`] from the connection or the `ROUTE_INPUT`
/// acks; an unknown pane id surfaces as [`AttachError::Refused`] from the
/// server.
pub async fn send_to(socket: &Path, pane: TerminalId, keys: &[String]) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    route_keys(&mut conn, &pane, keys).await
}

/// Paste a payload into a pre-resolved `pane` via the side-effect-free
/// `ROUTE_INPUT` route.
///
/// The paste sibling of [`send_to`]: exactly one [`InputEvent::Paste`]
/// carrying `data` and `trust` rides `ROUTE_INPUT`, so nothing attaches,
/// nothing subscribes, and the live pane keeps its dimensions. The server
/// owns the encoding: when the pane's program has bracketed paste (DEC
/// mode 2004) switched on, the payload is wrapped in `ESC[200~`/`ESC[201~`
/// markers; otherwise the bytes are delivered raw (`docs/spec/input.md`
/// §5).
///
/// Trust is the caller's claim about the payload: a
/// [`PasteTrust::Trusted`] paste is forwarded verbatim, while a
/// [`PasteTrust::Untrusted`] one is classified server-side and the pane's
/// untrusted-paste policy (reject by default) may drop it silently — the
/// `ROUTE_INPUT` still acks `Ok`.
///
/// # Errors
///
/// Propagates [`AttachError`] from the connection or the `ROUTE_INPUT`
/// ack; an unknown pane id surfaces as [`AttachError::Refused`] from the
/// server.
pub async fn paste_to(
    socket: &Path,
    pane: TerminalId,
    data: Vec<u8>,
    trust: PasteTrust,
) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    // Ignoring the interleave is safe here for the same reason the whole
    // module is side-effect-free: this connection never attaches or
    // subscribes, so no fanout reaches its mailbox, and the server's
    // `handle_route_input` (or the input lane's `route_command`) emits
    // nothing to the caller ahead of the ack — the pane's resulting bytes go
    // to that pane's *subscribers*, which this caller deliberately is not.
    match conn
        .request(
            1,
            Command::RouteInput {
                terminal_id: pane,
                event: InputEvent::Paste(PasteEvent { trust, data }),
            },
        )
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

/// Deliver each built [`InputEvent`] to `pane` over `conn` via `ROUTE_INPUT`.
///
/// Each `ROUTE_INPUT` is acked by a `COMMAND_RESULT` the server emits in
/// order, so the events land on the pane actor's input mailbox in send
/// order — no drain race.
async fn route_keys(
    conn: &mut Connection,
    pane: &TerminalId,
    keys: &[String],
) -> Result<(), AttachError> {
    for (i, event) in events_for(keys).into_iter().enumerate() {
        // request_id 1 may have been a preceding GET_STATE; route-input
        // acks start at 2 so a shared connection never reuses an id.
        let request_id = u32::try_from(i).unwrap_or(u32::MAX - 1).saturating_add(2);
        // Unsubscribed connection + a handler that pushes nothing before the
        // ack; see `paste_to` for the full argument.
        match conn
            .request(
                request_id,
                Command::RouteInput {
                    terminal_id: pane.clone(),
                    event,
                },
            )
            .await?
            .into_result_ignoring_interleaved()
        {
            CommandResult::Ok => {}
            CommandResult::Error { message, .. } => return Err(AttachError::Refused(message)),
            other => {
                return Err(AttachError::Protocol(crate::explain::explain_unexpected(
                    "ROUTE_INPUT",
                    &other,
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_map_to_control_bytes() {
        assert_eq!(spec_to_bytes("Enter"), b"\r");
        assert_eq!(spec_to_bytes("tab"), b"\t");
        assert_eq!(spec_to_bytes("Escape"), vec![0x1b]);
        assert_eq!(spec_to_bytes("Up"), b"\x1b[A");
    }

    #[test]
    fn control_and_alt_combos() {
        assert_eq!(spec_to_bytes("C-c"), vec![0x03]);
        assert_eq!(spec_to_bytes("c-a"), vec![0x01]);
        assert_eq!(spec_to_bytes("M-x"), vec![0x1b, b'x']);
    }

    #[test]
    fn literal_text_passes_through() {
        assert_eq!(spec_to_bytes("echo hi"), b"echo hi");
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
