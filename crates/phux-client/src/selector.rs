//! Client-side selector parsing and resolution (ADR-0021).
//!
//! The CLI's `TARGET` grammar names sessions, windows, and panes, none of
//! which are wire concepts, so selectors resolve client-side against a
//! `GET_STATE` snapshot to concrete [`ResourceId`]s.
//!
//! | Form        | Meaning                                             |
//! |-------------|-----------------------------------------------------|
//! | `.`         | the focused session (snapshot's `focused_session`)  |
//! | `=`         | unsupported for headless clients (no focus history) |
//! | `name`      | a session by name                                   |
//! | `name:N`    | window index `N` of session `name`                  |
//! | `name:tag`  | window whose name is `tag` in session `name`        |
//! | `name:N.M`  | pane index `M` of window `N` of session `name`      |
//! | `@N`        | an opaque local Terminal id (`ResourceId::local(N)`) |
//! | `host/@N`   | an opaque satellite Terminal id owned by `host`      |
//! | `#tag`      | every Terminal carrying L3 tag `tag` (`phux.tags/v1`) |
//! | `%name`     | the one agent named `name` (ADR-0075); see [`resolve_agent`] |
//!
//! `host` is an opaque token (any UTF-8, even `/@` or empty); parsing splits
//! at the final `/@` so the canonical formatter round-trips. `@N` and
//! `host/@N` resolve a resource of any kind; every other form resolves
//! Terminal-kind resources only, so an `AgentSession` never shifts a pane
//! index (ADR-0102).

use phux_protocol::ids::ResourceId;
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
    ResourceId(u32),
    /// `host/@N` — a Terminal addressed by its hub-qualified wire id.
    SatelliteResourceId {
        /// Opaque hub-local satellite routing token.
        host: String,
        /// Terminal id in the satellite server's local id space.
        id: u32,
    },
    /// `#tag` — every Terminal carrying the L3 tag `tag`; see
    /// [`resolve_with_tags`].
    Tag(String),
    /// `%name` — one agent (ADR-0075), resolved only by [`resolve_agent`];
    /// the set-valued [`resolve_with_tags`] yields nothing for it.
    Agent(String),
}

impl Selector {
    /// The one id an explicit `@N` / `host/@N` names, taken as given (no
    /// snapshot lookup); `None` for every other form.
    #[must_use]
    pub fn explicit_id(&self) -> Option<ResourceId> {
        match self {
            Self::ResourceId(id) => Some(ResourceId::local(*id)),
            Self::SatelliteResourceId { host, id } => {
                Some(ResourceId::satellite(host.as_str(), *id))
            }
            _ => None,
        }
    }
}

/// The selector as a user would write it: `parse(&selector.to_string())`
/// gives `selector` back, so an error can name the target it missed.
impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Current => f.write_str("."),
            Self::Session(name) => f.write_str(name),
            Self::Window(name, window) => write!(f, "{name}:{window}"),
            Self::Pane(name, window, pane) => write!(f, "{name}:{window}.{pane}"),
            Self::ResourceId(id) => write!(f, "@{id}"),
            Self::SatelliteResourceId { host, id } => write!(f, "{host}/@{id}"),
            Self::Tag(tag) => write!(f, "#{tag}"),
            Self::Agent(name) => write!(f, "%{name}"),
        }
    }
}

impl std::fmt::Display for WindowRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index(index) => write!(f, "{index}"),
            Self::Tag(tag) => f.write_str(tag),
        }
    }
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
    BadResourceId(String),
    /// A pane index `M` (after the `.`) was non-numeric or out of range.
    BadPaneIndex(String),
    /// `#` carried no tag (the bare sigil).
    EmptyTag,
    /// `%` carried no agent name (the bare sigil).
    EmptyAgentName,
    /// `%name` carried a name outside `^[a-z][a-z0-9_-]{0,31}$`.
    BadAgentName(String),
    /// `=` requires an attached client's local focus history.
    LastUnsupported,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty selector"),
            Self::BadResourceId(s) => write!(f, "invalid terminal id in '@{s}'"),
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
    // `%name` first, so `%a/@1` is a bad agent name rather than a satellite
    // id: `%name` is hub-local (ADR-0075 point 2).
    if let Some(name) = raw.strip_prefix('%') {
        if name.is_empty() {
            return Err(ParseError::EmptyAgentName);
        }
        if !is_addressable_agent_name(name) {
            return Err(ParseError::BadAgentName(name.to_owned()));
        }
        return Ok(Selector::Agent(name.to_owned()));
    }
    // The final `/@` before local `@N`: a host may itself begin with `@`,
    // contain `/@`, or be empty.
    if let Some((host, rest)) = raw.rsplit_once("/@") {
        let id = rest
            .parse::<u32>()
            .map_err(|_| ParseError::BadResourceId(rest.to_owned()))?;
        return Ok(Selector::SatelliteResourceId {
            host: host.to_owned(),
            id,
        });
    }
    if let Some(rest) = raw.strip_prefix('@') {
        let id = rest
            .parse::<u32>()
            .map_err(|_| ParseError::BadResourceId(rest.to_owned()))?;
        return Ok(Selector::ResourceId(id));
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

    // The first `.` separates the window locus from an optional pane index.
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

/// Whether `name` is spellable after `%`: `^[a-z][a-z0-9_-]{0,31}$`
/// (ADR-0075 point 4), narrower than the record's own `name` field.
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

/// Resolve a parsed [`Selector`] to the [`ResourceId`]s it names; empty on
/// a miss.
#[must_use]
pub fn resolve(selector: &Selector, snapshot: &SessionSnapshot) -> Vec<ResourceId> {
    resolve_with_tags(selector, snapshot, &TagIndex::new())
}

/// A map from `ResourceId` to its L3 tags (`phux.tags/v1`).
pub type TagIndex = std::collections::HashMap<ResourceId, Vec<String>>;

/// Like [`resolve`], but a `#tag` yields every Terminal carrying the tag in
/// `tags`, in snapshot order (ADR-0027).
///
/// [`Selector::Agent`] yields nothing, on purpose: callers follow this with
/// [`pick_target_pane`], which must never narrow `%name` to an arbitrary pane
/// (ADR-0075 point 3). Branch to [`resolve_agent`] first.
#[must_use]
pub fn resolve_with_tags(
    selector: &Selector,
    snapshot: &SessionSnapshot,
    tags: &TagIndex,
) -> Vec<ResourceId> {
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
        Selector::ResourceId(id) => resolve_wire_id(snapshot, ResourceId::local(*id)),
        Selector::SatelliteResourceId { host, id } => {
            resolve_wire_id(snapshot, ResourceId::satellite(host.as_str(), *id))
        }
        Selector::Tag(tag) => crate::resource::terminals(snapshot)
            .map(|p| p.id.clone())
            .filter(|id| tags.get(id).is_some_and(|ts| ts.iter().any(|t| t == tag)))
            .collect(),
        Selector::Agent(_) => Vec::new(),
    }
}

/// The Terminal-scoped `phux.agent/v1` records read back, plus whether all
/// of them were read.
///
/// [`resolve_agent`] refuses a partial index (ADR-0075 point 3), so
/// [`Default`] is the partial, empty, fail-closed value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentIndex {
    records: std::collections::HashMap<ResourceId, AgentRecord>,
    complete: bool,
}

impl AgentIndex {
    /// An index whose builder visited every pane in the snapshot and read a
    /// definite answer (a record, or a well-formed absence) for each.
    #[must_use]
    pub const fn complete(records: std::collections::HashMap<ResourceId, AgentRecord>) -> Self {
        Self {
            records,
            complete: true,
        }
    }

    /// An index whose builder gave up early; its records are a lower bound.
    #[must_use]
    pub const fn partial(records: std::collections::HashMap<ResourceId, AgentRecord>) -> Self {
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

    /// The record for one Terminal, if the index holds one.
    #[must_use]
    pub fn get(&self, id: &ResourceId) -> Option<&AgentRecord> {
        self.records.get(id)
    }
}

/// What `%name` resolves to: the one Terminal carrying the name, and its
/// live `AgentSession` child when it has exactly one (ADR-0103).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTarget {
    /// The Terminal whose `phux.agent/v1` record carries the name.
    pub terminal: ResourceId,
    /// Its unique live `AgentSession` child, when the server serves one.
    pub session: Option<ResourceId>,
}

/// Why a `%name` selector did not resolve to one agent: every variant is a
/// refusal to guess, with its ADR-0075 exit code from [`Self::exit_code`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentResolveError {
    /// No live hub-local pane carries that name (exit 1).
    Unknown {
        /// The name that was typed after `%`.
        name: String,
    },
    /// Two or more live records share the name (exit 2).
    Ambiguous {
        /// The name that was typed after `%`.
        name: String,
        /// Every Terminal carrying it, in snapshot order.
        candidates: Vec<ResourceId>,
    },
    /// A candidate's `name` equals its own `kind`: a per-kind manifest
    /// constant, not a chosen name (exit 2), refused even when unique.
    KindConstant {
        /// The name that was typed after `%`.
        name: String,
        /// Every Terminal carrying it, in snapshot order.
        candidates: Vec<ResourceId>,
    },
    /// The agent index is partial, so no answer is sound (exit 3).
    PartialIndex {
        /// The name that was typed after `%`.
        name: String,
        /// What the truncated index did match, in snapshot order.
        matched: Vec<ResourceId>,
    },
    /// The record has the withdrawn shape (ADR-0075 point 5); input verbs
    /// refuse (exit 2).
    Withdrawn {
        /// The name that was typed after `%`.
        name: String,
        /// The Terminal the name resolved to.
        terminal: ResourceId,
    },
    /// The Terminal has several live `AgentSession` children (exit 2).
    AmbiguousSession {
        /// The name that was typed after `%`.
        name: String,
        /// The Terminal the name resolved to.
        terminal: ResourceId,
        /// Every live session child, in snapshot order.
        candidates: Vec<ResourceId>,
    },
}

impl AgentResolveError {
    /// The exit code (ADR-0075 point 3): `1` miss, `2` refusal, `3` phux
    /// could not answer.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Unknown { .. } => 1,
            Self::Ambiguous { .. }
            | Self::KindConstant { .. }
            | Self::Withdrawn { .. }
            | Self::AmbiguousSession { .. } => 2,
            Self::PartialIndex { .. } => 3,
        }
    }

    /// The stable `--json` error code (`docs/consumers/agents.md` §7) every
    /// surface reports this refusal with, CLI and MCP alike.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "no_such_target",
            Self::Ambiguous { .. } | Self::AmbiguousSession { .. } => "selector_not_single",
            Self::KindConstant { .. } => "invalid_agent_name",
            Self::Withdrawn { .. } => "agent_withdrawn",
            Self::PartialIndex { .. } => "partial_view",
        }
    }

    /// What to do about this refusal, one line.
    #[must_use]
    pub const fn remedy(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => {
                "`phux agent list` shows every declared name; set one with `phux agent set \
                 TARGET --name <name>`"
            }
            Self::Ambiguous { .. } | Self::AmbiguousSession { .. } => {
                "address one candidate directly by @N"
            }
            Self::KindConstant { .. } => {
                "name one pane with `phux agent set @N --name <name>` and address that"
            }
            Self::Withdrawn { .. } => {
                "inspect the pane with `phux agent explain`; address it by @N to write anyway"
            }
            Self::PartialIndex { .. } => "retry once the fleet is whole, or address the pane by @N",
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
            | Self::Withdrawn { name, .. }
            | Self::AmbiguousSession { name, .. } => name,
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
            Self::AmbiguousSession {
                name,
                terminal,
                candidates,
            } => write!(
                f,
                "agent '{name}' ({}) has {} live agent sessions ({}); address one by @N",
                format_terminal_id(terminal),
                candidates.len(),
                render_candidates(candidates)
            ),
        }
    }
}

impl std::error::Error for AgentResolveError {}

fn render_candidates(candidates: &[ResourceId]) -> String {
    candidates
        .iter()
        .map(format_terminal_id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `record` has the withdrawn shape: a `kind` it kept, and
/// `state: unknown` (ADR-0075 point 5). No `kind` with `state: unknown` is an
/// identity-only declaration, not a withdrawal.
#[must_use]
pub fn is_withdrawn_agent_record(record: &AgentRecord) -> bool {
    record.kind.is_some() && record.state == AgentMetaState::Unknown
}

/// Resolve `%name` to exactly one agent (with its unique live session
/// child, if any), or refuse; candidates are enumerated in snapshot order.
///
/// Checks run in the order their conclusions are sound: kind-constant and
/// ambiguity hold even for a partial index, so they precede "could not
/// answer".
///
/// # Errors
///
/// [`AgentResolveError`] — see its variants; [`AgentResolveError::exit_code`]
/// maps each to its process status.
pub fn resolve_agent(
    name: &str,
    snapshot: &SessionSnapshot,
    index: &AgentIndex,
) -> Result<AgentTarget, AgentResolveError> {
    let candidates: Vec<ResourceId> = crate::resource::terminals(snapshot)
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
        // An unseen pane could hold this name too.
        return Err(AgentResolveError::PartialIndex {
            name: name.to_owned(),
            matched: candidates,
        });
    }
    let terminal = candidates
        .into_iter()
        .next()
        .ok_or_else(|| AgentResolveError::Unknown {
            name: name.to_owned(),
        })?;
    let sessions: Vec<ResourceId> = crate::resource::children_of(snapshot, &terminal)
        .map(|info| info.id.clone())
        .collect();
    let session = match sessions.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        _ => {
            return Err(AgentResolveError::AmbiguousSession {
                name: name.to_owned(),
                terminal,
                candidates: sessions,
            });
        }
    };
    Ok(AgentTarget { terminal, session })
}

/// [`resolve_agent`] plus the ADR-0075 point 5 write guard, for verbs that
/// deliver input: also refuses [`AgentResolveError::Withdrawn`].
///
/// # Errors
///
/// Every [`resolve_agent`] error, plus [`AgentResolveError::Withdrawn`].
pub fn resolve_agent_for_input(
    name: &str,
    snapshot: &SessionSnapshot,
    index: &AgentIndex,
) -> Result<AgentTarget, AgentResolveError> {
    let target = resolve_agent(name, snapshot, index)?;
    if index
        .get(&target.terminal)
        .is_some_and(is_withdrawn_agent_record)
    {
        return Err(AgentResolveError::Withdrawn {
            name: name.to_owned(),
            terminal: target.terminal,
        });
    }
    Ok(target)
}

/// What `%name` makes of `terminal`'s record, for `phux agent list` to say
/// which names `%` cannot address (ADR-0075 point 4).
///
/// `Ok("%name")` when [`resolve_agent`] resolves the record's name to
/// exactly `terminal`; otherwise the `--json` code `%name` would refuse with
/// ([`AgentResolveError::code`], or `invalid_agent_name` for a name outside
/// the `%` grammar). `None` when `terminal` has no record.
#[must_use]
pub fn agent_address(
    terminal: &ResourceId,
    snapshot: &SessionSnapshot,
    index: &AgentIndex,
) -> Option<Result<String, &'static str>> {
    let name = &index.get(terminal)?.name;
    if !is_addressable_agent_name(name) {
        return Some(Err("invalid_agent_name"));
    }
    Some(match resolve_agent(name, snapshot, index) {
        Ok(target) if &target.terminal == terminal => Ok(format!("%{name}")),
        // `terminal` is not a live Terminal in `snapshot`, so the name
        // reaches some other pane: never label this one with it.
        Ok(_) => Err("selector_not_single"),
        Err(err) => Err(err.code()),
    })
}

/// Whether this record's `name` is its own `kind` (ASCII case-insensitive):
/// the per-kind manifest constant a detector rule writes.
fn is_kind_constant(record: &AgentRecord) -> bool {
    record
        .kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case(&record.name))
}

/// The live session a whole-session selector (`.` or `name`) targets, so
/// `phux kill` can tear it down in one round trip; `None` for every
/// narrower form.
#[must_use]
pub fn whole_session_name(selector: &Selector, snapshot: &SessionSnapshot) -> Option<String> {
    let session_id = match selector {
        Selector::Current => snapshot.focused_session,
        Selector::Session(name) => session_id_by_name(snapshot, name)?,
        Selector::Window(..)
        | Selector::Pane(..)
        | Selector::ResourceId(_)
        | Selector::SatelliteResourceId { .. }
        | Selector::Tag(_)
        | Selector::Agent(_) => return None,
    };
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == session_id)
        .map(|s| s.name.clone())
}

/// Render a wire id as the canonical direct selector (`@N` or `host/@N`),
/// which round-trips through [`parse`].
#[must_use]
pub fn format_terminal_id(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{}/@{id}", host.as_str()),
    }
}

/// Choose one pane from a selector's `candidates`: the `focused` one if
/// present, else the first; `None` on a miss.
#[must_use]
pub fn pick_target_pane(candidates: &[ResourceId], focused: &ResourceId) -> Option<ResourceId> {
    candidates
        .iter()
        .find(|id| *id == focused)
        .or_else(|| candidates.first())
        .cloned()
}

fn resolve_wire_id(snapshot: &SessionSnapshot, wanted: ResourceId) -> Vec<ResourceId> {
    snapshot
        .resources
        .iter()
        .any(|pane| pane.id == wanted)
        .then_some(wanted)
        .into_iter()
        .collect()
}

/// All Terminal-kind resources in `session`, in snapshot order.
fn terminals_in_session(
    snapshot: &SessionSnapshot,
    session: phux_protocol::ids::SessionId,
) -> Vec<ResourceId> {
    let window_ids: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|w| w.session_id == session)
        .map(|w| w.id)
        .collect();
    crate::resource::terminals(snapshot)
        .filter(|p| window_ids.contains(&p.window_id))
        .map(|p| p.id.clone())
        .collect()
}

/// Terminals in the window `name`/`window` names, in snapshot order.
fn resolve_window(snapshot: &SessionSnapshot, name: &str, window: &WindowRef) -> Vec<ResourceId> {
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
        crate::resource::terminals(snapshot)
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
    /// Every canonical form prints back as written, so a miss can name it.
    #[test]
    fn display_round_trips_through_parse() {
        for raw in [
            ".",
            "work",
            "work:2",
            "work:logs",
            "work:2.1",
            "work:logs.0",
            "@7",
            "build-box/@3",
            "#ci",
            "%claude",
        ] {
            let selector = super::parse(raw).expect(raw);
            assert_eq!(selector.to_string(), raw);
        }
    }

    use super::*;
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};

    /// The session-name rule (`rename::check_session_name`) is exactly "the
    /// selector parses back to this session": names it accepts round-trip,
    /// and every non-blank name it refuses parses as something else.
    #[test]
    fn session_name_rule_matches_the_selector_grammar() {
        let names = [
            "work",
            ".config",
            "a.b",
            "a/b",
            "a b",
            "x@y",
            "a=b",
            "w-2",
            ".",
            "=",
            "@3",
            "@x",
            "#tag",
            "#",
            "%agent",
            "%",
            "x:y",
            ":",
            "a:1.2",
            "devbox/@7",
            "/@1",
        ];
        for name in names {
            let addressable =
                matches!(parse(name), Ok(Selector::Session(ref parsed)) if parsed == name);
            assert_eq!(
                crate::rename::check_session_name(name).is_ok(),
                addressable,
                "{name:?}"
            );
        }
    }

    #[test]
    fn an_explicit_id_is_taken_as_given() {
        assert_eq!(
            parse("@9").unwrap().explicit_id(),
            Some(ResourceId::local(9))
        );
        assert_eq!(
            parse("edge/@4").unwrap().explicit_id(),
            Some(ResourceId::satellite("edge", 4))
        );
        assert_eq!(parse("work").unwrap().explicit_id(), None);
    }

    #[test]
    fn parse_accepts_every_form() {
        let session = |s: &str| s.to_owned();
        for (raw, expected) in [
            (".", Selector::Current),
            ("work", Selector::Session(session("work"))),
            (
                "work:1",
                Selector::Window(session("work"), WindowRef::Index(1)),
            ),
            (
                "work:editor",
                Selector::Window(session("work"), WindowRef::Tag(session("editor"))),
            ),
            (
                "work:1.2",
                Selector::Pane(session("work"), WindowRef::Index(1), 2),
            ),
            ("@42", Selector::ResourceId(42)),
            (
                "devbox/@42",
                Selector::SatelliteResourceId {
                    host: session("devbox"),
                    id: 42,
                },
            ),
            (
                "/@1",
                Selector::SatelliteResourceId {
                    host: String::new(),
                    id: 1,
                },
            ),
            ("#build", Selector::Tag(session("build"))),
            ("%build", Selector::Agent(session("build"))),
            ("%r2-d2_9", Selector::Agent(session("r2-d2_9"))),
        ] {
            assert_eq!(parse(raw).unwrap(), expected, "{raw}");
        }
    }

    #[test]
    fn parse_rejects_malformed_selectors() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert!(matches!(parse("@nope"), Err(ParseError::BadResourceId(_))));
        assert!(matches!(
            parse("devbox/@nope"),
            Err(ParseError::BadResourceId(_))
        ));
        assert!(matches!(
            parse("work:1.x"),
            Err(ParseError::BadPaneIndex(_))
        ));
        assert_eq!(parse("#"), Err(ParseError::EmptyTag));
        assert_eq!(parse("="), Err(ParseError::LastUnsupported));
        assert_eq!(parse("%"), Err(ParseError::EmptyAgentName));
        // Hub-local: `%a/@1` is a bad agent name, never a satellite id.
        let longest = "a".repeat(AGENT_NAME_MAX_LEN);
        assert_eq!(
            parse(&format!("%{longest}")).unwrap(),
            Selector::Agent(longest.clone())
        );
        for bad in [
            "%a/@1".to_owned(),
            "%Build".to_owned(),
            "%9lives".to_owned(),
            "%-lead".to_owned(),
            "%_lead".to_owned(),
            "%my agent".to_owned(),
            "%añejo".to_owned(),
            format!("%{longest}a"),
        ] {
            assert!(
                matches!(parse(&bad), Err(ParseError::BadAgentName(_))),
                "{bad}"
            );
        }
        assert!(is_addressable_agent_name("build"));
        assert!(!is_addressable_agent_name("Build Runner"));
        assert!(!is_addressable_agent_name(""));
    }

    /// Session "work" (id 1) with windows 0 (@100) and 1 (@101, @102), plus
    /// session "play" (@200).
    fn fixture() -> SessionSnapshot {
        let work = SessionId::new(1);
        let play = SessionId::new(2);
        let w0 = WindowId::new(10);
        let w1 = WindowId::new(11);
        let p0 = WindowId::new(20);
        SessionSnapshot::new(work, w0, ResourceId::local(100))
            .with_sessions(vec![
                SessionInfo::new(work, "work"),
                SessionInfo::new(play, "play"),
            ])
            .with_windows(vec![
                WindowInfo::new(w0, work, "shell").with_index(0),
                WindowInfo::new(w1, work, "editor").with_index(1),
                WindowInfo::new(p0, play, "shell").with_index(0),
            ])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(100), w0, 80, 24),
                ResourceInfo::new(ResourceId::local(101), w1, 80, 24),
                ResourceInfo::new(ResourceId::local(102), w1, 80, 24),
                ResourceInfo::new(ResourceId::local(200), p0, 80, 24),
            ])
    }

    fn ids(locals: &[u32]) -> Vec<ResourceId> {
        locals.iter().map(|id| ResourceId::local(*id)).collect()
    }

    #[test]
    fn resolve_maps_each_form_against_the_snapshot() {
        let mut snap = fixture();
        snap.resources.push(ResourceInfo::new(
            ResourceId::satellite("devbox", 7),
            WindowId::new(999),
            120,
            40,
        ));
        for (raw, expected) in [
            (".", ids(&[100, 101, 102])),
            ("work", ids(&[100, 101, 102])),
            ("work:1", ids(&[101, 102])),
            ("work:editor", ids(&[101, 102])),
            ("work:1.1", ids(&[102])),
            ("@100", ids(&[100])),
            ("devbox/@7", vec![ResourceId::satellite("devbox", 7)]),
            // Direct ids still require inventory membership.
            ("other/@7", vec![]),
            ("devbox/@8", vec![]),
            ("@999", vec![]),
            ("ghost", vec![]),
        ] {
            assert_eq!(resolve(&parse(raw).unwrap(), &snap), expected, "{raw}");
        }
    }

    #[test]
    fn resolve_tag_returns_every_tagged_terminal_in_snapshot_order() {
        let snap = fixture();
        let mut tags = TagIndex::new();
        tags.insert(
            ResourceId::local(100),
            vec!["build".to_owned(), "ci".to_owned()],
        );
        tags.insert(ResourceId::local(200), vec!["build".to_owned()]);
        tags.insert(ResourceId::local(101), vec!["web".to_owned()]);

        let build = resolve_with_tags(&parse("#build").unwrap(), &snap, &tags);
        assert_eq!(build, ids(&[100, 200]));
        assert!(resolve_with_tags(&parse("#nope").unwrap(), &snap, &tags).is_empty());
        assert!(resolve(&parse("#build").unwrap(), &snap).is_empty());
    }

    #[test]
    fn terminal_id_formatter_emits_parseable_canonical_selectors() {
        for id in [
            ResourceId::local(7),
            ResourceId::satellite("devbox", 42),
            ResourceId::satellite("", 43),
            ResourceId::satellite("region/@rack/@node", 44),
            ResourceId::satellite("日本語 /@ host", 45),
            ResourceId::satellite("@prod", 46),
        ] {
            let rendered = format_terminal_id(&id);
            let mut snap = fixture();
            snap.resources
                .push(ResourceInfo::new(id.clone(), WindowId::new(999), 80, 24));
            assert_eq!(resolve(&parse(&rendered).unwrap(), &snap), vec![id]);
        }
    }

    #[test]
    fn pick_target_pane_prefers_focused_then_first_then_none() {
        let [a, b, c] = [1, 2, 3].map(ResourceId::local);
        assert_eq!(
            pick_target_pane(&[a.clone(), b.clone()], &b),
            Some(b.clone())
        );
        assert_eq!(pick_target_pane(&[a.clone(), c], &b), Some(a));
        assert_eq!(pick_target_pane(&[], &b), None);
    }

    // ---- ADR-0075: `%name` agent addressing -----------------------------

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
            .map(|(id, rec)| (ResourceId::local(*id), rec.clone()))
            .collect();
        if complete {
            AgentIndex::complete(map)
        } else {
            AgentIndex::partial(map)
        }
    }

    /// ADR-0075 point 4: `phux agent list` says which names `%` cannot
    /// address, with the code `%name` would refuse with.
    #[test]
    fn agent_address_says_which_listed_names_percent_cannot_reach() {
        let snap = fixture();
        let index = index_of(
            &[
                (100, record("build", None, AgentMetaState::Working)),
                (101, record("Code Review", None, AgentMetaState::Idle)),
                (102, record("twin", None, AgentMetaState::Idle)),
                (200, record("twin", None, AgentMetaState::Idle)),
            ],
            true,
        );
        let address = |id| agent_address(&ResourceId::local(id), &snap, &index);
        assert_eq!(address(100), Some(Ok("%build".to_owned())));
        assert_eq!(address(101), Some(Err("invalid_agent_name")));
        assert_eq!(address(102), Some(Err("selector_not_single")));
        assert_eq!(address(200), Some(Err("selector_not_single")));
        assert_eq!(address(999), None, "no record, nothing to address");

        let constant = index_of(
            &[(100, record("claude", Some("claude"), AgentMetaState::Idle))],
            true,
        );
        assert_eq!(
            agent_address(&ResourceId::local(100), &snap, &constant),
            Some(Err("invalid_agent_name")),
            "a per-kind constant is listed but not addressable, even when unique",
        );

        let partial = index_of(
            &[(100, record("build", None, AgentMetaState::Working))],
            false,
        );
        assert_eq!(
            agent_address(&ResourceId::local(100), &snap, &partial),
            Some(Err("partial_view")),
        );
    }

    /// Every refusal carries the contract code and remedy each surface (CLI,
    /// spatial, MCP) reports it with.
    #[test]
    fn agent_resolve_errors_carry_their_contract_codes() {
        let t = ResourceId::local(1);
        for (err, code, exit) in [
            (
                AgentResolveError::Unknown { name: "a".into() },
                "no_such_target",
                1,
            ),
            (
                AgentResolveError::Ambiguous {
                    name: "a".into(),
                    candidates: vec![],
                },
                "selector_not_single",
                2,
            ),
            (
                AgentResolveError::AmbiguousSession {
                    name: "a".into(),
                    terminal: t.clone(),
                    candidates: vec![],
                },
                "selector_not_single",
                2,
            ),
            (
                AgentResolveError::KindConstant {
                    name: "a".into(),
                    candidates: vec![],
                },
                "invalid_agent_name",
                2,
            ),
            (
                AgentResolveError::Withdrawn {
                    name: "a".into(),
                    terminal: t,
                },
                "agent_withdrawn",
                2,
            ),
            (
                AgentResolveError::PartialIndex {
                    name: "a".into(),
                    matched: vec![],
                },
                "partial_view",
                3,
            ),
        ] {
            assert_eq!(err.code(), code, "{err:?}");
            assert_eq!(err.exit_code(), exit, "{err:?}");
            assert!(!err.remedy().is_empty(), "{err:?}");
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
            AgentTarget {
                terminal: ResourceId::local(101),
                session: None,
            }
        );
        assert_eq!(
            resolve_agent_for_input("review", &snap, &index)
                .unwrap()
                .terminal,
            ResourceId::local(200)
        );
    }

    /// A miss is exit 1, an ambiguity exit 2 (candidates in snapshot order),
    /// and a truncated index exit 3 even for a single or empty match.
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
                candidates: ids(&[100, 200]),
            }
        );
        assert_eq!(ambiguous.exit_code(), 2);
        let rendered = ambiguous.to_string();
        assert!(
            rendered.contains("@100") && rendered.contains("@200"),
            "{rendered}"
        );

        let truncated = index_of(
            &[(101, record("build", None, AgentMetaState::Working))],
            false,
        );
        let partial = resolve_agent("build", &snap, &truncated).unwrap_err();
        assert_eq!(
            partial,
            AgentResolveError::PartialIndex {
                name: "build".to_owned(),
                matched: ids(&[101]),
            }
        );
        assert_eq!(partial.exit_code(), 3);
        let nothing = resolve_agent("ghost", &snap, &truncated).unwrap_err();
        assert_eq!(nothing.exit_code(), 3);
        // The default index is partial, so it fails closed the same way.
        let default = resolve_agent("build", &snap, &AgentIndex::default()).unwrap_err();
        assert_eq!(default.exit_code(), 3);
    }

    /// A manifest kind constant (`name == kind`) refuses on one pane exactly
    /// as on many, whoever wrote it; a name merely containing a kind is fine.
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
                candidates: ids(&[101]),
            }
        );
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("phux agent set"), "{err}");

        let fleet = index_of(
            &[
                (
                    100,
                    record("claude", Some("claude"), AgentMetaState::Working),
                ),
                (102, record("claude", Some("claude"), AgentMetaState::Idle)),
            ],
            true,
        );
        let err = resolve_agent("claude", &snap, &fleet).unwrap_err();
        assert!(matches!(err, AgentResolveError::KindConstant { .. }));
        assert!(err.to_string().contains("@102"));

        let by_hand = index_of(
            &[(101, record("codex", Some("Codex"), AgentMetaState::Idle))],
            true,
        );
        assert!(matches!(
            resolve_agent("codex", &snap, &by_hand),
            Err(AgentResolveError::KindConstant { .. })
        ));

        let chosen = index_of(
            &[(
                101,
                record("claude-review", Some("claude"), AgentMetaState::Working),
            )],
            true,
        );
        assert_eq!(
            resolve_agent("claude-review", &snap, &chosen)
                .unwrap()
                .terminal,
            ResourceId::local(101)
        );
    }

    /// The write guard is the withdrawn shape (a `kind` and `state:
    /// unknown`), not bare `state == unknown`; read verbs skip the gate.
    #[test]
    fn input_verbs_refuse_only_the_withdrawn_shape() {
        let snap = fixture();
        let withdrawn = index_of(
            &[(
                101,
                record("build", Some("claude"), AgentMetaState::Unknown),
            )],
            true,
        );
        assert_eq!(
            resolve_agent("build", &snap, &withdrawn).unwrap().terminal,
            ResourceId::local(101),
        );
        let err = resolve_agent_for_input("build", &snap, &withdrawn).unwrap_err();
        assert_eq!(
            err,
            AgentResolveError::Withdrawn {
                name: "build".to_owned(),
                terminal: ResourceId::local(101),
            }
        );
        assert_eq!(err.exit_code(), 2);

        let identity_only = index_of(
            &[(101, record("build", None, AgentMetaState::Unknown))],
            true,
        );
        assert_eq!(
            resolve_agent_for_input("build", &snap, &identity_only)
                .unwrap()
                .terminal,
            ResourceId::local(101)
        );
        assert!(!is_withdrawn_agent_record(&record(
            "build",
            Some("claude"),
            AgentMetaState::Idle
        )));
    }

    /// `%name` must never reach `pick_target_pane`: the set-valued seam
    /// yields nothing for it, and it is never a whole-session teardown.
    #[test]
    fn the_shared_set_valued_seam_never_narrows_an_agent_selector() {
        let snap = fixture();
        let sel = parse("%build").unwrap();
        assert!(resolve(&sel, &snap).is_empty());
        assert_eq!(
            pick_target_pane(&resolve(&sel, &snap), &snap.focused_resource),
            None
        );
        assert_eq!(whole_session_name(&sel, &snap), None);
    }

    // ---- ADR-0102 / ADR-0103: resource kinds in the selector -------------

    /// Pane 101 hosts agent session @901; pane 102 hosts @902 and @903.
    fn kinded_fixture() -> SessionSnapshot {
        use phux_protocol::ids::ResourceKind;
        let mut snap = fixture();
        for (session, parent) in [(901, 101), (902, 102), (903, 102)] {
            snap.resources.push(
                ResourceInfo::new(ResourceId::local(session), WindowId::new(11), 0, 0)
                    .with_kind(ResourceKind::AgentSession)
                    .with_parent(Some(ResourceId::local(parent))),
            );
        }
        snap
    }

    /// Set-valued selectors see Terminal-kind resources only (a pane index
    /// counts panes); `@N` resolves any kind.
    #[test]
    fn set_valued_selectors_resolve_terminal_kind_only() {
        let snap = kinded_fixture();
        for (raw, expected) in [
            ("work", ids(&[100, 101, 102])),
            ("work:1", ids(&[101, 102])),
            ("work:1.1", ids(&[102])),
            ("work:1.2", vec![]),
            ("@901", ids(&[901])),
        ] {
            assert_eq!(resolve(&parse(raw).unwrap(), &snap), expected, "{raw}");
        }
        let mut tags = TagIndex::new();
        tags.insert(ResourceId::local(901), vec!["build".to_owned()]);
        tags.insert(ResourceId::local(101), vec!["build".to_owned()]);
        assert_eq!(
            resolve_with_tags(&parse("#build").unwrap(), &snap, &tags),
            ids(&[101]),
            "a tag on a session resource is not a pane match"
        );
    }

    /// `%name` carries the Terminal's unique live session and refuses when
    /// it has several.
    #[test]
    fn resolve_agent_carries_the_unique_session_child_and_refuses_several() {
        let snap = kinded_fixture();
        let index = index_of(
            &[
                (
                    101,
                    record("reviewer", Some("claude"), AgentMetaState::Working),
                ),
                (102, record("builder", Some("claude"), AgentMetaState::Idle)),
                (100, record("solo", None, AgentMetaState::Idle)),
            ],
            true,
        );
        assert_eq!(
            resolve_agent("reviewer", &snap, &index).unwrap(),
            AgentTarget {
                terminal: ResourceId::local(101),
                session: Some(ResourceId::local(901)),
            }
        );
        assert_eq!(
            resolve_agent("solo", &snap, &index).unwrap(),
            AgentTarget {
                terminal: ResourceId::local(100),
                session: None,
            },
        );
        let err = resolve_agent("builder", &snap, &index).unwrap_err();
        assert_eq!(
            err,
            AgentResolveError::AmbiguousSession {
                name: "builder".to_owned(),
                terminal: ResourceId::local(102),
                candidates: ids(&[902, 903]),
            }
        );
        assert_eq!(err.exit_code(), 2);
    }
}
