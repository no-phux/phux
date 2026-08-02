//! Client-side selector parsing and resolution (phux-3kj, ADR-0021).
//!
//! The CLI's `TARGET` grammar (`docs/consumers/tui.md` §3) names sessions,
//! windows, and panes — none of which are wire concepts (ADR-0017). Per
//! [ADR-0021](../../../ADR/0021-control-plane-commands.md) selectors are
//! therefore resolved **client-side** against a `GET_STATE` snapshot: a
//! selector resolves to a concrete set of [`TerminalId`]s, and only those
//! Terminal-scoped ids are sent back to the server (e.g. one
//! `KILL_TERMINAL` per resolved Terminal). The server never parses a
//! selector and never learns the words "session" or "window".
//!
//! Grammar:
//!
//! | Form        | Meaning                                             |
//! |-------------|-----------------------------------------------------|
//! | `.`         | the focused session (snapshot's `focused_session`)  |
//! | `=`         | unsupported for headless clients (no focus history) |
//! | `name`      | a session by name                                   |
//! | `name:N`    | window index `N` of session `name`                  |
//! | `name:tag`  | window whose name is `tag` in session `name`        |
//! | `name:N.M`  | pane index `M` of window `N` of session `name`      |
//! | `@N`        | an opaque local Terminal id (`TerminalId::local(N)`) |
//! | `host/@N`   | an opaque satellite Terminal id owned by `host`      |
//! | `#tag`      | every Terminal carrying L3 tag `tag` (`phux.tags/v1`) |
//!
//! `host` is an opaque registry token and may contain any UTF-8 text,
//! including `/@` or be empty. Parsing uses the final `/@` delimiter so the
//! canonical formatter round-trips every registry-accepted host token.
//!
//! The `#tag` form ([ADR-0027](../../../ADR/0027-terminal-references-and-l3-links.md)
//! decision point 5) resolves to a *set*, like a session name, against L3
//! tag metadata the caller fetches alongside the snapshot — see
//! [`resolve_with_tags`]. The server stays selector-agnostic
//! ([ADR-0017](../../../ADR/0017-tui-not-protocol-privileged.md)).

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::info::SessionSnapshot;

/// A parsed selector. Resolution happens later against a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// `.` — the focused session.
    Current,
    /// `name` — a whole session.
    Session(String),
    /// `name:N` or `name:tag` — one window of a session.
    Window(String, WindowRef),
    /// `name:N.M` or `name:tag.M` — one pane of one window.
    Pane(String, WindowRef, u16),
    /// `@N` — a Terminal addressed directly by its local wire id.
    TerminalId(u32),
    /// `host/@N` — a Terminal addressed by its hub-qualified wire id.
    SatelliteTerminalId {
        /// Opaque hub-local satellite routing token.
        host: String,
        /// Terminal id in the satellite server's local id space.
        id: u32,
    },
    /// `#tag` — every Terminal carrying the L3 tag `tag` (`phux.tags/v1`).
    /// Resolves to a set; see [`resolve_with_tags`].
    Tag(String),
}

/// How a window is addressed within a session: by numeric index or by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowRef {
    /// `N` — position in the session's windows list.
    Index(u16),
    /// `tag` — the window's name.
    Tag(String),
}

/// Why a selector string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The selector was empty.
    Empty,
    /// `@N` or `host/@N` carried a non-numeric or out-of-range id.
    BadTerminalId(String),
    /// A pane index `M` (after the `.`) was non-numeric or out of range.
    BadPaneIndex(String),
    /// `#` carried no tag (the bare sigil).
    EmptyTag,
    /// `=` requires an attached client's local focus history.
    LastUnsupported,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty selector"),
            Self::BadTerminalId(s) => write!(f, "invalid terminal id in '@{s}'"),
            Self::BadPaneIndex(s) => write!(f, "invalid pane index '{s}'"),
            Self::EmptyTag => write!(f, "empty tag in '#' selector"),
            Self::LastUnsupported => write!(
                f,
                "'=' requires attached-TUI focus history; use '.' or an explicit target"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse one `TARGET` string into a [`Selector`].
///
/// # Errors
///
/// Returns [`ParseError`] for an empty selector or a malformed numeric
/// component (`@N` id, pane index).
pub fn parse(raw: &str) -> Result<Selector, ParseError> {
    if raw.is_empty() {
        return Err(ParseError::Empty);
    }
    if raw == "." {
        return Ok(Selector::Current);
    }
    if raw == "=" {
        return Err(ParseError::LastUnsupported);
    }
    // Split at the final delimiter before recognizing local `@N`: a
    // SatelliteHost is an opaque registry token and may itself begin with `@`,
    // contain `/@`, or be empty. Since the formatter appends exactly one
    // `/@N` suffix, rsplit makes every accepted host reversible.
    if let Some((host, rest)) = raw.rsplit_once("/@") {
        let id = rest
            .parse::<u32>()
            .map_err(|_| ParseError::BadTerminalId(rest.to_owned()))?;
        return Ok(Selector::SatelliteTerminalId {
            host: host.to_owned(),
            id,
        });
    }
    if let Some(rest) = raw.strip_prefix('@') {
        let id = rest
            .parse::<u32>()
            .map_err(|_| ParseError::BadTerminalId(rest.to_owned()))?;
        return Ok(Selector::TerminalId(id));
    }
    if let Some(tag) = raw.strip_prefix('#') {
        if tag.is_empty() {
            return Err(ParseError::EmptyTag);
        }
        return Ok(Selector::Tag(tag.to_owned()));
    }

    // `name`, `name:window`, or `name:window.pane`.
    let Some((name, locus)) = raw.split_once(':') else {
        return Ok(Selector::Session(raw.to_owned()));
    };
    let name = name.to_owned();

    // Split the window locus from an optional `.pane` suffix. Only the
    // FIRST `.` separates window from pane, so a window tag may itself
    // contain dots in a future grammar; today windows are `window-N`.
    if let Some((window_part, pane_part)) = locus.split_once('.') {
        let window = parse_window_ref(window_part);
        let pane = pane_part
            .parse::<u16>()
            .map_err(|_| ParseError::BadPaneIndex(pane_part.to_owned()))?;
        Ok(Selector::Pane(name, window, pane))
    } else {
        Ok(Selector::Window(name, parse_window_ref(locus)))
    }
}

/// A window locus is an [`WindowRef::Index`] when fully numeric, else a
/// [`WindowRef::Tag`].
fn parse_window_ref(part: &str) -> WindowRef {
    part.parse::<u16>()
        .map_or_else(|_| WindowRef::Tag(part.to_owned()), WindowRef::Index)
}

/// Resolve a parsed [`Selector`] to the [`TerminalId`]s it names.
///
/// Returns an empty vec when the selector matches nothing (e.g. an unknown
/// session) — callers decide whether that is an error (`kill` treats it as
/// a selector miss).
#[must_use]
pub fn resolve(selector: &Selector, snapshot: &SessionSnapshot) -> Vec<TerminalId> {
    resolve_with_tags(selector, snapshot, &TagIndex::new())
}

/// A map from `TerminalId` to its L3 tags (`phux.tags/v1`).
///
/// The caller fetches it alongside the snapshot. `resolve_with_tags` reads it
/// only for a [`Selector::Tag`]; an empty map resolves every `#tag` to nothing.
pub type TagIndex = std::collections::HashMap<TerminalId, Vec<String>>;

/// Like [`resolve`], but resolves a [`Selector::Tag`] against `tags`.
///
/// Every selector form except `#tag` ignores `tags` entirely (so
/// [`resolve`] is the zero-tag specialization). A `#tag` selector yields, in
/// snapshot order, every Terminal whose `tags` entry contains the tag — the
/// set semantics ADR-0027 specifies, matching how a session name resolves to
/// many Terminals.
#[must_use]
pub fn resolve_with_tags(
    selector: &Selector,
    snapshot: &SessionSnapshot,
    tags: &TagIndex,
) -> Vec<TerminalId> {
    match selector {
        Selector::Current => terminals_in_session(snapshot, snapshot.focused_session),
        Selector::Session(name) => session_id_by_name(snapshot, name)
            .map_or_else(Vec::new, |sid| terminals_in_session(snapshot, sid)),
        Selector::Window(name, window) => resolve_window(snapshot, name, window),
        Selector::Pane(name, window, pane_index) => {
            let panes = resolve_window(snapshot, name, window);
            panes
                .into_iter()
                .nth(*pane_index as usize)
                .into_iter()
                .collect()
        }
        Selector::TerminalId(id) => resolve_wire_id(snapshot, TerminalId::local(*id)),
        Selector::SatelliteTerminalId { host, id } => {
            resolve_wire_id(snapshot, TerminalId::satellite(host.as_str(), *id))
        }
        Selector::Tag(tag) => snapshot
            .panes
            .iter()
            .map(|p| p.id.clone())
            .filter(|id| tags.get(id).is_some_and(|ts| ts.iter().any(|t| t == tag)))
            .collect(),
    }
}

/// The name of the whole session this selector targets, if any.
///
/// Returns `Some(name)` for selectors that address an entire session —
/// `Current` (resolved against `snapshot.focused_session`) and `Session(name)` —
/// and `None` for window / pane / terminal-id
/// selectors, which address a strict subset and must stay per-Terminal.
///
/// This is the seam `phux kill` uses to collapse a whole-session teardown
/// into a single `KILL_COLLECTION` round-trip while keeping sub-session
/// targets on the per-`KILL_TERMINAL` path (`phux-h9s`, ADR-0021 §3). The
/// name is returned only when it resolves to a live session in `snapshot`,
/// so the caller can rely on it existing server-side.
#[must_use]
pub fn whole_session_name(selector: &Selector, snapshot: &SessionSnapshot) -> Option<String> {
    let session_id = match selector {
        Selector::Current => snapshot.focused_session,
        Selector::Session(name) => session_id_by_name(snapshot, name)?,
        Selector::Window(..)
        | Selector::Pane(..)
        | Selector::TerminalId(_)
        | Selector::SatelliteTerminalId { .. }
        | Selector::Tag(_) => return None,
    };
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == session_id)
        .map(|s| s.name.clone())
}

/// Render a wire id as the canonical direct Terminal selector.
///
/// Local ids use `@N`; satellite ids retain their opaque hub-routing token
/// and use `host/@N`. Both forms round-trip through [`parse`] + [`resolve`].
#[must_use]
pub fn format_terminal_id(id: &TerminalId) -> String {
    match id {
        TerminalId::Local { id } => format!("@{id}"),
        TerminalId::Satellite { host, id } => format!("{}/@{id}", host.as_str()),
    }
}

/// Choose one pane from a selector's `candidates`.
///
/// Prefers the one equal to the server's `focused` pane (the common "the
/// session I'm looking at" case), else the first in snapshot order. `None`
/// only when the selector matched nothing. Shared by the CLI and MCP tools.
#[must_use]
pub fn pick_target_pane(candidates: &[TerminalId], focused: &TerminalId) -> Option<TerminalId> {
    candidates
        .iter()
        .find(|id| *id == focused)
        .or_else(|| candidates.first())
        .cloned()
}

fn resolve_wire_id(snapshot: &SessionSnapshot, wanted: TerminalId) -> Vec<TerminalId> {
    snapshot
        .panes
        .iter()
        .any(|pane| pane.id == wanted)
        .then_some(wanted)
        .into_iter()
        .collect()
}

/// All Terminals in `session`, across every window, in snapshot order.
fn terminals_in_session(
    snapshot: &SessionSnapshot,
    session: phux_protocol::ids::SessionId,
) -> Vec<TerminalId> {
    let window_ids: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|w| w.session_id == session)
        .map(|w| w.id)
        .collect();
    snapshot
        .panes
        .iter()
        .filter(|p| window_ids.contains(&p.window_id))
        .map(|p| p.id.clone())
        .collect()
}

/// Terminals in the window `name`/`window` names, in snapshot order.
fn resolve_window(snapshot: &SessionSnapshot, name: &str, window: &WindowRef) -> Vec<TerminalId> {
    let Some(sid) = session_id_by_name(snapshot, name) else {
        return Vec::new();
    };
    let window_id = snapshot
        .windows
        .iter()
        .filter(|w| w.session_id == sid)
        .find(|w| match window {
            WindowRef::Index(n) => w.index == *n,
            WindowRef::Tag(tag) => w.name == *tag,
        })
        .map(|w| w.id);
    window_id.map_or_else(Vec::new, |wid| {
        snapshot
            .panes
            .iter()
            .filter(|p| p.window_id == wid)
            .map(|p| p.id.clone())
            .collect()
    })
}

fn session_id_by_name(
    snapshot: &SessionSnapshot,
    name: &str,
) -> Option<phux_protocol::ids::SessionId> {
    snapshot
        .sessions
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{SessionInfo, TerminalInfo, WindowInfo};

    #[test]
    fn parse_session_window_pane_and_terminal_forms() {
        assert_eq!(parse(".").unwrap(), Selector::Current);
        assert_eq!(parse("work").unwrap(), Selector::Session("work".to_owned()));
        assert_eq!(
            parse("work:1").unwrap(),
            Selector::Window("work".to_owned(), WindowRef::Index(1)),
        );
        assert_eq!(
            parse("work:editor").unwrap(),
            Selector::Window("work".to_owned(), WindowRef::Tag("editor".to_owned())),
        );
        assert_eq!(
            parse("work:1.2").unwrap(),
            Selector::Pane("work".to_owned(), WindowRef::Index(1), 2),
        );
        assert_eq!(parse("@42").unwrap(), Selector::TerminalId(42));
        assert_eq!(
            parse("devbox/@42").unwrap(),
            Selector::SatelliteTerminalId {
                host: "devbox".to_owned(),
                id: 42,
            },
        );
        assert_eq!(parse("#build").unwrap(), Selector::Tag("build".to_owned()));
    }

    #[test]
    fn parse_rejects_empty_and_bad_numbers() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert!(matches!(parse("@nope"), Err(ParseError::BadTerminalId(_))));
        assert!(matches!(
            parse("devbox/@nope"),
            Err(ParseError::BadTerminalId(_))
        ));
        assert_eq!(
            parse("/@1").unwrap(),
            Selector::SatelliteTerminalId {
                host: String::new(),
                id: 1,
            }
        );
        assert!(matches!(
            parse("work:1.x"),
            Err(ParseError::BadPaneIndex(_))
        ));
        assert_eq!(parse("#"), Err(ParseError::EmptyTag));
        assert_eq!(parse("="), Err(ParseError::LastUnsupported));
    }

    #[test]
    fn resolve_tag_returns_every_tagged_terminal_in_snapshot_order() {
        let snap = fixture();
        // Tag 'build' on panes 100 (work) and 200 (play) — a cross-session set.
        let mut tags = TagIndex::new();
        tags.insert(
            TerminalId::local(100),
            vec!["build".to_owned(), "ci".to_owned()],
        );
        tags.insert(TerminalId::local(200), vec!["build".to_owned()]);
        tags.insert(TerminalId::local(101), vec!["web".to_owned()]);

        let build = resolve_with_tags(&parse("#build").unwrap(), &snap, &tags);
        assert_eq!(build, vec![TerminalId::local(100), TerminalId::local(200)]);

        let ci = resolve_with_tags(&parse("#ci").unwrap(), &snap, &tags);
        assert_eq!(ci, vec![TerminalId::local(100)]);

        // An unknown tag, and the no-index path, both resolve to nothing.
        assert!(resolve_with_tags(&parse("#nope").unwrap(), &snap, &tags).is_empty());
        assert!(resolve(&parse("#build").unwrap(), &snap).is_empty());
    }

    /// Build a snapshot: session "work" (id 1) with two windows, each
    /// holding panes, plus a second session "play" (id 2).
    fn fixture() -> SessionSnapshot {
        let work = SessionId::new(1);
        let play = SessionId::new(2);
        let w0 = WindowId::new(10);
        let w1 = WindowId::new(11);
        let p0 = WindowId::new(20);
        let sessions = vec![
            SessionInfo::new(work, "work"),
            SessionInfo::new(play, "play"),
        ];
        let windows = vec![
            WindowInfo::new(w0, work, "shell").with_index(0),
            WindowInfo::new(w1, work, "editor").with_index(1),
            WindowInfo::new(p0, play, "shell").with_index(0),
        ];
        let panes = vec![
            TerminalInfo::new(TerminalId::local(100), w0, 80, 24),
            TerminalInfo::new(TerminalId::local(101), w1, 80, 24),
            TerminalInfo::new(TerminalId::local(102), w1, 80, 24),
            TerminalInfo::new(TerminalId::local(200), p0, 80, 24),
        ];
        SessionSnapshot::new(work, w0, TerminalId::local(100))
            .with_sessions(sessions)
            .with_windows(windows)
            .with_panes(panes)
    }

    #[test]
    fn resolve_session_returns_all_its_terminals() {
        let snap = fixture();
        let sel = parse("work").unwrap();
        let ids = resolve(&sel, &snap);
        assert_eq!(
            ids,
            vec![
                TerminalId::local(100),
                TerminalId::local(101),
                TerminalId::local(102),
            ],
        );
    }

    #[test]
    fn resolve_window_by_index_and_tag() {
        let snap = fixture();
        let by_index = resolve(&parse("work:1").unwrap(), &snap);
        let by_tag = resolve(&parse("work:editor").unwrap(), &snap);
        let expected = vec![TerminalId::local(101), TerminalId::local(102)];
        assert_eq!(by_index, expected);
        assert_eq!(by_tag, expected);
    }

    #[test]
    fn resolve_pane_picks_one_terminal() {
        let snap = fixture();
        let ids = resolve(&parse("work:1.1").unwrap(), &snap);
        assert_eq!(ids, vec![TerminalId::local(102)]);
    }

    #[test]
    fn resolve_terminal_id_and_focused_and_misses() {
        let mut snap = fixture();
        snap.panes.push(TerminalInfo::new(
            TerminalId::satellite("devbox", 7),
            WindowId::new(999),
            120,
            40,
        ));
        assert_eq!(
            resolve(&parse("@100").unwrap(), &snap),
            vec![TerminalId::local(100)],
        );
        assert_eq!(
            resolve(&parse("devbox/@7").unwrap(), &snap),
            vec![TerminalId::satellite("devbox", 7)],
        );
        // Direct ids still require inventory membership; a wrong host or id misses.
        assert!(resolve(&parse("other/@7").unwrap(), &snap).is_empty());
        assert!(resolve(&parse("devbox/@8").unwrap(), &snap).is_empty());
        // Unknown local terminal id → empty.
        assert!(resolve(&parse("@999").unwrap(), &snap).is_empty());
        // Unknown session → empty.
        assert!(resolve(&parse("ghost").unwrap(), &snap).is_empty());
        // `.` resolves to the focused session ("work").
        assert_eq!(
            resolve(&Selector::Current, &snap),
            vec![
                TerminalId::local(100),
                TerminalId::local(101),
                TerminalId::local(102),
            ],
        );
    }

    #[test]
    fn terminal_id_formatter_emits_parseable_canonical_selectors() {
        for id in [
            TerminalId::local(7),
            TerminalId::satellite("devbox", 42),
            TerminalId::satellite("", 43),
            TerminalId::satellite("region/@rack/@node", 44),
            TerminalId::satellite("日本語 /@ host", 45),
            TerminalId::satellite("@prod", 46),
        ] {
            let rendered = format_terminal_id(&id);
            let mut snap = fixture();
            snap.panes
                .push(TerminalInfo::new(id.clone(), WindowId::new(999), 80, 24));
            assert_eq!(resolve(&parse(&rendered).unwrap(), &snap), vec![id]);
        }
    }

    #[test]
    fn pick_target_pane_prefers_focused_then_first_then_none() {
        let a = TerminalId::local(1);
        let b = TerminalId::local(2);
        let c = TerminalId::local(3);
        // Focused is among the candidates → pick it, not the first.
        assert_eq!(
            pick_target_pane(&[a.clone(), b.clone()], &b),
            Some(b.clone())
        );
        // Focused not among candidates → first in snapshot order.
        assert_eq!(pick_target_pane(&[a.clone(), c], &b), Some(a));
        // Empty candidate set → None (a selector miss).
        assert_eq!(pick_target_pane(&[], &b), None);
    }
}
