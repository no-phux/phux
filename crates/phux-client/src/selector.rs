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
//! | `%name`     | the one hub-local Terminal whose `phux.agent/v1` name is `name` |
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
//!
//! The `%name` form ([ADR-0075](../../../ADR/0075-agent-name-addressing.md))
//! is the odd one out: it is the only selector that resolves to **exactly
//! one** Terminal *or refuses*. It therefore does **not** go through
//! [`resolve_with_tags`] — see [`resolve_agent`] and
//! [`resolve_agent_for_input`], and read the warning on
//! [`resolve_with_tags`] before adding a caller.

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::info::SessionSnapshot;

use crate::agent_meta::{AgentMetaState, AgentRecord};

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
    /// `%name` — the one hub-local Terminal whose `phux.agent/v1` record is
    /// named `name` (ADR-0075).
    ///
    /// Singular by construction: it resolves through [`resolve_agent`], which
    /// yields one [`TerminalId`] or an [`AgentResolveError`]. It deliberately
    /// resolves to nothing through the set-valued [`resolve_with_tags`] seam,
    /// so a caller that has not been taught the singular form gets a selector
    /// miss instead of an arbitrary pane.
    Agent(String),
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
    /// `%` carried no agent name (the bare sigil).
    EmptyAgentName,
    /// `%name` carried a name outside the addressable grammar
    /// `^[a-z][a-z0-9_-]{0,31}$` (ADR-0075 point 4). Checked at parse time so
    /// a typo fails locally, before any round trip.
    BadAgentName(String),
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
            Self::EmptyAgentName => write!(f, "empty agent name in '%' selector"),
            Self::BadAgentName(s) => write!(
                f,
                "'%{s}' is not an addressable agent name (lowercase letter, then \
                 up to {} more of [a-z0-9_-]); set one with `phux agent set TARGET --name <name>`",
                AGENT_NAME_MAX_LEN - 1
            ),
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
    // `%name` is checked before every other prefix rule. `%` is free in the
    // grammar and shell-safe unquoted (ADR-0075 "Why"), and no other accepted
    // form begins with it — the cost, which the ADR names, is that a session
    // literally called `%foo` becomes unaddressable. Checking it here also
    // means `%a/@1` fails as a bad agent name rather than being swallowed by
    // the satellite rsplit below: `%name` is hub-local (ADR-0075 point 2).
    if let Some(name) = raw.strip_prefix('%') {
        if name.is_empty() {
            return Err(ParseError::EmptyAgentName);
        }
        if !is_addressable_agent_name(name) {
            return Err(ParseError::BadAgentName(name.to_owned()));
        }
        return Ok(Selector::Agent(name.to_owned()));
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

/// Longest agent name `%` can address (ADR-0075 point 4:
/// `^[a-z][a-z0-9_-]{0,31}$`).
pub const AGENT_NAME_MAX_LEN: usize = 32;

/// Whether `name` is spellable after `%`.
///
/// The addressable grammar is `^[a-z][a-z0-9_-]{0,31}$` (ADR-0075 point 4).
/// It is deliberately *narrower* than the record's own `name` field, which
/// `docs/spec/L3.md` §3.7 leaves as "any non-empty string": a display-style
/// name stays valid, listed, and addressable by `@N` — just not by `%`. This
/// predicate is public so `phux agent list` can say which of the names it
/// prints are addressable.
///
/// There is no write-time check. L3 is last-writer-wins, so a scan two racing
/// writers both pass is an `O(panes)` round trip and not a guarantee
/// (ADR-0075 point 4); refusal at resolve is the enforcing check.
#[must_use]
pub fn is_addressable_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    name.len() <= AGENT_NAME_MAX_LEN
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
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
///
/// # `%name` resolves to nothing here, on purpose
///
/// This function is the set-valued seam, and every caller of it follows up
/// with [`pick_target_pane`] — the CLI's `resolve_target`, `spawn`, the
/// spatial verbs, and `phux-mcp`'s `resolve_one`, which applies it
/// unconditionally. `pick_target_pane` is right for a selector that asked for
/// a representative and catastrophic for one whose entire value is that it
/// names one thing (ADR-0075 point 3: `phux send-keys %build 'rm -rf .'` must
/// never land in an arbitrary pane that shares a label).
///
/// Since this signature has nowhere to put a refusal and no agent index to
/// read, [`Selector::Agent`] yields an empty set: an un-migrated caller gets a
/// selector **miss**, which is fail-closed. Callers that mean to support
/// `%name` must branch on [`Selector::Agent`] *before* reaching here and call
/// [`resolve_agent`] (or [`resolve_agent_for_input`]) instead.
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
        // Singular selector, set-valued seam: see this function's docs. Never
        // let `%name` reach `pick_target_pane`.
        Selector::Agent(_) => Vec::new(),
    }
}

/// The `phux.agent/v1` records the CLI read back, plus whether it managed to
/// read *all* of them.
///
/// The completeness bit is the whole point (ADR-0075 point 3). The index is
/// built with one `GET_METADATA` per pane, and the existing builder is
/// best-effort by design: a mid-flight transport failure returns what it
/// collected so heuristic consumers degrade instead of erroring. That is
/// exactly wrong for `%name` — a truncated index turns a real ambiguity into a
/// confident single match, and turns "we did not finish looking" into "no pane
/// holds that name". [`resolve_agent`] refuses a partial index rather than
/// answer from it.
///
/// [`Default`] is therefore the *partial*, empty index: the fail-closed value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentIndex {
    records: std::collections::HashMap<TerminalId, AgentRecord>,
    complete: bool,
}

impl AgentIndex {
    /// An index whose builder visited every pane in the snapshot and read a
    /// definite answer (a record, or a well-formed absence) for each.
    #[must_use]
    pub const fn complete(records: std::collections::HashMap<TerminalId, AgentRecord>) -> Self {
        Self {
            records,
            complete: true,
        }
    }

    /// An index whose builder gave up early — a dropped connection, a refused
    /// read, a hub that could not reach a satellite. Whatever it holds is a
    /// lower bound.
    #[must_use]
    pub const fn partial(records: std::collections::HashMap<TerminalId, AgentRecord>) -> Self {
        Self {
            records,
            complete: false,
        }
    }

    /// Whether every pane was accounted for.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// The records, keyed by the Terminal they are scoped to.
    #[must_use]
    pub const fn records(&self) -> &std::collections::HashMap<TerminalId, AgentRecord> {
        &self.records
    }

    /// The record for one Terminal, if the index holds one.
    #[must_use]
    pub fn get(&self, id: &TerminalId) -> Option<&AgentRecord> {
        self.records.get(id)
    }
}

/// Why a `%name` selector did not resolve to one Terminal.
///
/// Every variant is a *refusal to guess*. The exit codes are ADR-0075 point 3
/// and are reported by [`Self::exit_code`] so the CLI and MCP map them the
/// same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentResolveError {
    /// No live hub-local pane carries that name. A plain selector miss
    /// (exit 1), the same outcome as an unknown session or an unknown tag.
    ///
    /// `phux.agent/v1` does not federate, so a satellite agent lands here too:
    /// `%name` is hub-local by construction (ADR-0075 point 2).
    Unknown {
        /// The name that was typed after `%`.
        name: String,
    },
    /// Two or more live records share the name. Refuse (exit 2) and enumerate
    /// every candidate, so the operator can retarget by `@N` — never narrow.
    Ambiguous {
        /// The name that was typed after `%`.
        name: String,
        /// Every Terminal carrying it, in snapshot order.
        candidates: Vec<TerminalId>,
    },
    /// At least one candidate's `name` equals its own `kind` — the shape a
    /// per-*kind* manifest constant leaves, not a name a human chose
    /// (ADR-0075 point 4, phux-w7z2.25). Refuse (exit 2) with the fix named.
    ///
    /// This fires on one Claude pane exactly as on twelve: resolving the
    /// constant while a single one is up would be a target whose meaning
    /// silently changes when the second spawns. The record has no provenance
    /// field, so this is a shape test rather than a provenance read — a human
    /// who runs `phux agent set @7 --name claude --kind claude` is refused
    /// too, which the ADR accepts.
    KindConstant {
        /// The name that was typed after `%`.
        name: String,
        /// Every Terminal carrying it, in snapshot order.
        candidates: Vec<TerminalId>,
    },
    /// The agent index was not built completely, so a "one match" or "no
    /// match" answer would be taken against a narrower world than the caller
    /// assumed. Refuse as partial (exit 3).
    ///
    /// Exit 3 keeps the meaning `docs/reference/exit-codes.md` publishes —
    /// phux could not answer. Unlike a `1`, the target may well exist, so a
    /// retry is correct once the link is back.
    PartialIndex {
        /// The name that was typed after `%`.
        name: String,
        /// What the truncated index did match, in snapshot order.
        matched: Vec<TerminalId>,
    },
    /// The record resolved, but it carries a `kind` **and** `state: unknown` —
    /// exactly what a withdrawal leaves behind, so it is positive evidence
    /// that a producer which knew this pane gave the claim up (ADR-0075
    /// point 5). Input-delivering verbs refuse (exit 2); read-only verbs skip
    /// this gate entirely.
    Withdrawn {
        /// The name that was typed after `%`.
        name: String,
        /// The Terminal the name resolved to.
        terminal: TerminalId,
    },
}

impl AgentResolveError {
    /// The process exit code this refusal maps to (ADR-0075 point 3).
    ///
    /// `1` selector miss, `2` refusal, `3` phux could not answer. A verb that
    /// has already spent one of these — `run` mirrors its child's status —
    /// keeps its own contract and carries the distinction in the message,
    /// which ADR-0075 point 3 names explicitly.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Unknown { .. } => 1,
            Self::Ambiguous { .. } | Self::KindConstant { .. } | Self::Withdrawn { .. } => 2,
            Self::PartialIndex { .. } => 3,
        }
    }

    /// The name that was typed after `%`.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Unknown { name }
            | Self::Ambiguous { name, .. }
            | Self::KindConstant { name, .. }
            | Self::PartialIndex { name, .. }
            | Self::Withdrawn { name, .. } => name,
        }
    }
}

impl std::fmt::Display for AgentResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { name } => write!(f, "no agent named '{name}' on this hub"),
            Self::Ambiguous { name, candidates } => write!(
                f,
                "agent name '{name}' is ambiguous: {}; address one directly, \
                 or rename with `phux agent set @N --name <name>`",
                render_candidates(candidates)
            ),
            Self::KindConstant { name, candidates } => write!(
                f,
                "'{name}' is an agent kind, not a chosen name — every pane running \
                 that kind carries it ({}); name one with \
                 `phux agent set @N --name <name>` and address that",
                render_candidates(candidates)
            ),
            Self::PartialIndex { name, .. } => write!(
                f,
                "could not read every pane's agent record, so '{name}' is \
                 unresolved rather than absent; retry, or address the pane by @N"
            ),
            Self::Withdrawn { name, terminal } => write!(
                f,
                "agent '{name}' ({}) has a withdrawn record — its producer gave the \
                 claim up, so who is in that pane now is unknown; address it by @N \
                 to send anyway",
                format_terminal_id(terminal)
            ),
        }
    }
}

impl std::error::Error for AgentResolveError {}

fn render_candidates(candidates: &[TerminalId]) -> String {
    candidates
        .iter()
        .map(format_terminal_id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `record` has the *withdrawn shape*: a `kind` it kept, and
/// `state: unknown` (ADR-0075 point 5).
///
/// That pairing is what ADR-0046's retraction, a declaration withdrawn
/// because its occupant died, and an in-place occupant-change correction all
/// leave behind, and it is client-observable because `AgentRecord::kind` is
/// `skip_serializing_if = "Option::is_none"`. A record with **no** `kind` and
/// `state: unknown` is the resting value of an identity-only declaration
/// (`docs/spec/L3.md` §3.7: an absent `state` means unknown) and is *not*
/// withdrawn — gating on bare `state != unknown` would refuse
/// `phux agent set --name build` forever.
///
/// This is a **level** read, in the §3.7 sense: a non-`unknown` state asserts
/// only that no state-bearing rule contradicts it right now — absence of
/// contrary evidence, equally true of a crashed pane — never that the occupant
/// is still who you named. That is the right predicate for a "do not disturb
/// this" gate, and a crashed pane reading as do-not-disturb errs the safe way.
#[must_use]
pub fn is_withdrawn_agent_record(record: &AgentRecord) -> bool {
    record.kind.is_some() && record.state == AgentMetaState::Unknown
}

/// Resolve `%name` to exactly one Terminal, or refuse.
///
/// `pick_target_pane` is never applied: a name's entire value is that it names
/// one thing, so every non-singular outcome is a refusal that enumerates what
/// it saw (ADR-0075 point 3). Candidates are collected in `snapshot.panes`
/// order so the enumeration is deterministic rather than hash order.
///
/// The checks run in the order in which their conclusions are *sound*: the
/// kind-constant and ambiguity refusals hold whether or not the index finished
/// (more panes can only add candidates), so they are reported before the
/// weaker "could not answer".
///
/// # Errors
///
/// [`AgentResolveError`] — see its variants; [`AgentResolveError::exit_code`]
/// maps each to its process status.
pub fn resolve_agent(
    name: &str,
    snapshot: &SessionSnapshot,
    index: &AgentIndex,
) -> Result<TerminalId, AgentResolveError> {
    // Exact match on `name`. The record's own field is looser than the
    // addressable grammar (§3.7), so a display-style name is listed and
    // reachable by `@N` but never by `%` — ADR-0075 point 4's two grammars.
    let candidates: Vec<TerminalId> = snapshot
        .panes
        .iter()
        .map(|p| p.id.clone())
        .filter(|id| index.get(id).is_some_and(|rec| rec.name == name))
        .collect();

    if candidates
        .iter()
        .filter_map(|id| index.get(id))
        .any(is_kind_constant)
    {
        return Err(AgentResolveError::KindConstant {
            name: name.to_owned(),
            candidates,
        });
    }
    if candidates.len() > 1 {
        return Err(AgentResolveError::Ambiguous {
            name: name.to_owned(),
            candidates,
        });
    }
    if !index.is_complete() {
        // Sound conclusions are exhausted: an unseen pane could hold this
        // name too, so neither the single match nor the empty one is safe to
        // report. "Nothing matched" and "we did not finish looking" must not
        // collapse into one silence.
        return Err(AgentResolveError::PartialIndex {
            name: name.to_owned(),
            matched: candidates,
        });
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| AgentResolveError::Unknown {
            name: name.to_owned(),
        })
}

/// [`resolve_agent`] plus ADR-0075 point 5's write guard, for the verbs that
/// deliver input into the pane — `send-keys`, `paste`, `signal`, `run`, and
/// every ADR-0053 acknowledged-batch verb.
///
/// The extra refusal is [`AgentResolveError::Withdrawn`]. Read-only verbs
/// (`agent show`, `snapshot`, `capture`, `watch`) call [`resolve_agent`]
/// instead and skip it.
///
/// # Errors
///
/// Every [`resolve_agent`] error, plus [`AgentResolveError::Withdrawn`].
pub fn resolve_agent_for_input(
    name: &str,
    snapshot: &SessionSnapshot,
    index: &AgentIndex,
) -> Result<TerminalId, AgentResolveError> {
    let terminal = resolve_agent(name, snapshot, index)?;
    if index.get(&terminal).is_some_and(is_withdrawn_agent_record) {
        return Err(AgentResolveError::Withdrawn {
            name: name.to_owned(),
            terminal,
        });
    }
    Ok(terminal)
}

/// Whether this record's `name` is its own `kind` — the manifest-constant
/// shape a per-kind detector rule writes (`rules/claude.toml` sets both to
/// `claude`), which cannot distinguish one pane from another of the same kind.
///
/// ASCII-case-insensitive, matching how the shipped
/// `phux agent send-keys --expect-agent` compares names.
fn is_kind_constant(record: &AgentRecord) -> bool {
    record
        .kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case(&record.name))
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
        | Selector::Tag(_)
        // `%name` addresses one Terminal, never a session, so it must stay on
        // the per-`KILL_TERMINAL` path.
        | Selector::Agent(_) => return None,
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

    // ---- ADR-0075: `%name` agent addressing -----------------------------

    /// `%name` parses to its own variant without disturbing a neighbouring
    /// form: the sigil is checked first, so `%a/@1` is a bad agent name
    /// rather than a satellite id, and every pre-existing spelling still
    /// parses to exactly what it did before.
    #[test]
    fn parse_agent_sigil_slots_in_without_disturbing_the_other_forms() {
        assert_eq!(
            parse("%build").unwrap(),
            Selector::Agent("build".to_owned())
        );
        assert_eq!(
            parse("%r2-d2_9").unwrap(),
            Selector::Agent("r2-d2_9".to_owned())
        );
        // Every other documented form is untouched.
        assert_eq!(parse(".").unwrap(), Selector::Current);
        assert_eq!(parse("work").unwrap(), Selector::Session("work".to_owned()));
        assert_eq!(
            parse("work:1").unwrap(),
            Selector::Window("work".to_owned(), WindowRef::Index(1))
        );
        assert_eq!(
            parse("work:1.2").unwrap(),
            Selector::Pane("work".to_owned(), WindowRef::Index(1), 2)
        );
        assert_eq!(parse("@42").unwrap(), Selector::TerminalId(42));
        assert_eq!(
            parse("devbox/@42").unwrap(),
            Selector::SatelliteTerminalId {
                host: "devbox".to_owned(),
                id: 42,
            }
        );
        assert_eq!(parse("#build").unwrap(), Selector::Tag("build".to_owned()));
        // Hub-local: a `%name` never becomes a satellite id.
        assert!(matches!(parse("%a/@1"), Err(ParseError::BadAgentName(_))));
    }

    /// The addressable grammar is `^[a-z][a-z0-9_-]{0,31}$`, checked at parse
    /// time so a typo fails locally, before any round trip.
    #[test]
    fn parse_agent_name_enforces_the_addressable_grammar() {
        assert_eq!(parse("%"), Err(ParseError::EmptyAgentName));
        for bad in [
            "%Build",
            "%9lives",
            "%-lead",
            "%_lead",
            "%my agent",
            "%añejo",
        ] {
            assert!(
                matches!(parse(bad), Err(ParseError::BadAgentName(_))),
                "{bad} must not parse as an addressable agent name"
            );
        }
        let longest = "a".repeat(AGENT_NAME_MAX_LEN);
        assert_eq!(
            parse(&format!("%{longest}")).unwrap(),
            Selector::Agent(longest.clone())
        );
        assert!(matches!(
            parse(&format!("%{longest}a")),
            Err(ParseError::BadAgentName(_))
        ));
        // The predicate `agent list` uses agrees with the parser.
        assert!(is_addressable_agent_name("build"));
        assert!(!is_addressable_agent_name("Build Runner"));
        assert!(!is_addressable_agent_name(""));
    }

    fn record(name: &str, kind: Option<&str>, state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            kind: kind.map(str::to_owned),
            state,
            attention: None,
            session: None,
        }
    }

    fn index_of(entries: &[(u32, AgentRecord)], complete: bool) -> AgentIndex {
        let map: std::collections::HashMap<_, _> = entries
            .iter()
            .map(|(id, rec)| (TerminalId::local(*id), rec.clone()))
            .collect();
        if complete {
            AgentIndex::complete(map)
        } else {
            AgentIndex::partial(map)
        }
    }

    #[test]
    fn resolve_agent_returns_the_single_pane_that_chose_the_name() {
        let snap = fixture();
        let index = index_of(
            &[
                (101, record("build", None, AgentMetaState::Working)),
                (200, record("review", Some("codex"), AgentMetaState::Idle)),
            ],
            true,
        );
        assert_eq!(
            resolve_agent("build", &snap, &index).unwrap(),
            TerminalId::local(101)
        );
        // A declared record with a `kind` and a real state resolves for the
        // input verbs too — only the withdrawn shape is gated.
        assert_eq!(
            resolve_agent_for_input("review", &snap, &index).unwrap(),
            TerminalId::local(200)
        );
    }

    /// The three ADR-0075 point 3 failure modes stay distinct: a miss is
    /// exit 1, an ambiguity is exit 2 with every candidate enumerated in
    /// snapshot order, and a truncated index is exit 3 rather than a
    /// confident answer taken from a narrower world.
    #[test]
    fn resolve_agent_refuses_a_miss_an_ambiguity_and_a_partial_index_distinctly() {
        let snap = fixture();

        let complete = index_of(
            &[(101, record("build", None, AgentMetaState::Working))],
            true,
        );
        let miss = resolve_agent("ghost", &snap, &complete).unwrap_err();
        assert_eq!(
            miss,
            AgentResolveError::Unknown {
                name: "ghost".to_owned()
            }
        );
        assert_eq!(miss.exit_code(), 1);

        // Two live records share the name: refuse, enumerating both. Panes
        // 100 and 200 are in that snapshot order, not hash order.
        let shared = index_of(
            &[
                (200, record("build", None, AgentMetaState::Idle)),
                (100, record("build", None, AgentMetaState::Working)),
            ],
            true,
        );
        let ambiguous = resolve_agent("build", &snap, &shared).unwrap_err();
        assert_eq!(
            ambiguous,
            AgentResolveError::Ambiguous {
                name: "build".to_owned(),
                candidates: vec![TerminalId::local(100), TerminalId::local(200)],
            }
        );
        assert_eq!(ambiguous.exit_code(), 2);
        let rendered = ambiguous.to_string();
        assert!(rendered.contains("@100"), "{rendered}");
        assert!(rendered.contains("@200"), "{rendered}");

        // Exactly one match, but the index was truncated: an unseen pane could
        // hold the name too, so a single match must NOT be reported.
        let truncated = index_of(
            &[(101, record("build", None, AgentMetaState::Working))],
            false,
        );
        let partial = resolve_agent("build", &snap, &truncated).unwrap_err();
        assert_eq!(
            partial,
            AgentResolveError::PartialIndex {
                name: "build".to_owned(),
                matched: vec![TerminalId::local(101)],
            }
        );
        assert_eq!(partial.exit_code(), 3);
        // And "nothing matched" against a truncated index is the same refusal,
        // never the exit-1 miss.
        let nothing = resolve_agent("ghost", &snap, &truncated).unwrap_err();
        assert_eq!(nothing.exit_code(), 3);
    }

    /// phux-w7z2.25: the detector writes the manifest constant (`name` defaults
    /// to `kind`), which is per-kind where a name must be per-pane. `%claude`
    /// refuses as a kind constant on ONE such pane exactly as on twelve, and
    /// the message names `phux agent set --name` as the fix.
    #[test]
    fn resolve_agent_refuses_a_manifest_kind_constant_even_when_it_is_unique() {
        let snap = fixture();
        let lone = index_of(
            &[(
                101,
                record("claude", Some("claude"), AgentMetaState::Working),
            )],
            true,
        );
        let err = resolve_agent("claude", &snap, &lone).unwrap_err();
        assert_eq!(
            err,
            AgentResolveError::KindConstant {
                name: "claude".to_owned(),
                candidates: vec![TerminalId::local(101)],
            }
        );
        assert_eq!(err.exit_code(), 2);
        let rendered = err.to_string();
        assert!(rendered.contains("phux agent set"), "{rendered}");

        // A fleet of them refuses identically, with every candidate listed.
        let fleet = index_of(
            &[
                (
                    100,
                    record("claude", Some("claude"), AgentMetaState::Working),
                ),
                (101, record("claude", Some("claude"), AgentMetaState::Idle)),
                (102, record("claude", Some("claude"), AgentMetaState::Idle)),
            ],
            true,
        );
        let err = resolve_agent("claude", &snap, &fleet).unwrap_err();
        assert!(matches!(err, AgentResolveError::KindConstant { .. }));
        assert!(err.to_string().contains("@102"));

        // A human who spells the constant by hand is refused the same way —
        // the record has no provenance field, so this is a shape test.
        let by_hand = index_of(
            &[(101, record("codex", Some("Codex"), AgentMetaState::Idle))],
            true,
        );
        assert!(matches!(
            resolve_agent("codex", &snap, &by_hand),
            Err(AgentResolveError::KindConstant { .. })
        ));

        // A chosen name that merely *contains* a kind is fine.
        let chosen = index_of(
            &[(
                101,
                record("claude-review", Some("claude"), AgentMetaState::Working),
            )],
            true,
        );
        assert_eq!(
            resolve_agent("claude-review", &snap, &chosen).unwrap(),
            TerminalId::local(101)
        );
    }

    /// ADR-0075 point 5: the write guard is the WITHDRAWN SHAPE — a `kind`
    /// AND `state: unknown` — not bare `state != unknown`, which would refuse
    /// an identity-only declaration forever. Read verbs skip the gate.
    #[test]
    fn input_verbs_refuse_only_the_withdrawn_shape() {
        let snap = fixture();

        // Withdrawn: kept its kind, lost its state.
        let withdrawn = index_of(
            &[(
                101,
                record("build", Some("claude"), AgentMetaState::Unknown),
            )],
            true,
        );
        assert_eq!(
            resolve_agent("build", &snap, &withdrawn).unwrap(),
            TerminalId::local(101),
            "read-only verbs must still resolve a withdrawn record"
        );
        let err = resolve_agent_for_input("build", &snap, &withdrawn).unwrap_err();
        assert_eq!(
            err,
            AgentResolveError::Withdrawn {
                name: "build".to_owned(),
                terminal: TerminalId::local(101),
            }
        );
        assert_eq!(err.exit_code(), 2);

        // Identity-only declaration (`phux agent set --name build`): no kind,
        // resting state unknown. NOT withdrawn — this must keep working.
        let identity_only = index_of(
            &[(101, record("build", None, AgentMetaState::Unknown))],
            true,
        );
        assert_eq!(
            resolve_agent_for_input("build", &snap, &identity_only).unwrap(),
            TerminalId::local(101)
        );

        assert!(is_withdrawn_agent_record(&record(
            "build",
            Some("claude"),
            AgentMetaState::Unknown
        )));
        assert!(!is_withdrawn_agent_record(&record(
            "build",
            None,
            AgentMetaState::Unknown
        )));
        assert!(!is_withdrawn_agent_record(&record(
            "build",
            Some("claude"),
            AgentMetaState::Idle
        )));
    }

    /// ADR-0075 point 3: `%name` must never reach `pick_target_pane`. The
    /// shared set-valued seam has no agent index and nowhere to put a refusal,
    /// so it yields nothing — an un-migrated caller gets a miss, not a pane.
    #[test]
    fn the_shared_set_valued_seam_never_narrows_an_agent_selector() {
        let snap = fixture();
        let sel = parse("%build").unwrap();
        assert!(resolve(&sel, &snap).is_empty());
        assert!(resolve_with_tags(&sel, &snap, &TagIndex::new()).is_empty());
        // Which is exactly what makes `pick_target_pane` fail closed here.
        assert_eq!(
            pick_target_pane(&resolve(&sel, &snap), &snap.focused_pane),
            None
        );
        // And `%name` is a Terminal target, never a whole-session teardown.
        assert_eq!(whole_session_name(&sel, &snap), None);
    }

    /// A `Default` index is the fail-closed one: empty AND partial, so a
    /// caller that forgets to build it refuses rather than reporting a miss.
    #[test]
    fn a_default_agent_index_is_partial_not_empty_and_complete() {
        let snap = fixture();
        let index = AgentIndex::default();
        assert!(!index.is_complete());
        assert!(index.records().is_empty());
        assert_eq!(
            resolve_agent("build", &snap, &index)
                .unwrap_err()
                .exit_code(),
            3
        );
    }
}
