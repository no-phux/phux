//! Client-side selector parsing and resolution (ADR-0021).
//!
//! The CLI's `TARGET` grammar names sessions, arrangement groups, and panes,
//! so selectors resolve client-side against a `GET_STATE` snapshot, plus the
//! session's arrangement for group forms and the organization records for
//! key prefixes ([`ResolveContext`]), to concrete [`ResourceId`]s.
//!
//! | Form        | Meaning                                             |
//! |-------------|-----------------------------------------------------|
//! | `.`         | the focused session (snapshot's `focused_session`)  |
//! | `=`         | unsupported for headless clients (no focus history) |
//! | `name`      | a session by name                                   |
//! | `name:N`    | group `N` (0-based) of session `name`'s arrangement |
//! | `name:tag`  | the group named `tag` in session `name`             |
//! | `name:N.M`  | leaf `M` (depth-first, 0-based) of group `N`        |
//! | `@N`        | an opaque local Terminal id (`ResourceId::local(N)`) |
//! | `host/@N`   | an opaque satellite Terminal id owned by `host`      |
//! | `#tag`      | every Terminal carrying L3 tag `tag` (`phux.tags/v1`) |
//! | `%name`     | the one agent named `name` (ADR-0075); see [`resolve_agent`] |
//! | `^ab12cd`   | the session or resource whose key starts with `ab12cd` |
//!
//! Group forms read the session's arrangement when the caller supplies it;
//! without one (an older server, or a session never arranged) they fall back
//! to the legacy windows the snapshot lists. A key prefix needs the
//! organization records; it names a record even when nothing executes, but
//! resolves to Terminals only for what executes here.
//!
//! `host` is an opaque token (any UTF-8, even `/@` or empty); parsing splits
//! at the final `/@` so the canonical formatter round-trips. `@N` and
//! `host/@N` resolve a resource of any kind; every other form resolves
//! Terminal-kind resources only, so an `AgentSession` never shifts a pane
//! index (ADR-0102).

use phux_protocol::ids::{KEY_MIN_PREFIX, KEY_TEXT_LEN, ResourceId};

#[cfg(doc)]
use crate::org::OrgInventory;

mod agent;
mod resolve;

pub use agent::{
    AgentIndex, AgentResolveError, AgentTarget, agent_address, is_withdrawn_agent_record,
    resolve_agent, resolve_agent_for_input,
};
pub use resolve::{
    Arrangements, ResolveContext, TagIndex, format_terminal_id, pick_target_pane, resolve,
    resolve_in, resolve_with_tags, whole_session_name,
};

/// A parsed selector. Resolution happens later against a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// `.` — the focused session.
    Current,
    /// `name` — a whole session.
    Session(String),
    /// `name:N` or `name:tag` — one arrangement group of a session.
    Group(String, GroupRef),
    /// `name:N.M` or `name:tag.M` — one leaf of one group.
    Pane(String, GroupRef, u16),
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
    /// `^ab12cd` — the session or resource whose key starts with this
    /// prefix (lowercase base32, at least six digits); see
    /// [`OrgInventory::resolve_key_prefix`].
    Key(String),
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
            Self::Group(name, group) => write!(f, "{name}:{group}"),
            Self::Pane(name, window, pane) => write!(f, "{name}:{window}.{pane}"),
            Self::ResourceId(id) => write!(f, "@{id}"),
            Self::SatelliteResourceId { host, id } => write!(f, "{host}/@{id}"),
            Self::Tag(tag) => write!(f, "#{tag}"),
            Self::Agent(name) => write!(f, "%{name}"),
            Self::Key(prefix) => write!(f, "^{prefix}"),
        }
    }
}

impl std::fmt::Display for GroupRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index(index) => write!(f, "{index}"),
            Self::Tag(tag) => f.write_str(tag),
        }
    }
}

/// How a group is addressed within a session: by position or by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupRef {
    /// `N` — 0-based position in the session's arrangement.
    Index(u16),
    /// `tag` — the group's name.
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
    /// `^` carried something other than 6 to 26 base32 digits.
    BadKeyPrefix(String),
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
            Self::BadKeyPrefix(s) => write!(
                f,
                "'^{s}' is not a key prefix ({KEY_MIN_PREFIX} to {KEY_TEXT_LEN} base32 digits, \
                 a-z and 2-7)"
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
    if let Some(prefix) = raw.strip_prefix('^') {
        let valid = (KEY_MIN_PREFIX..=KEY_TEXT_LEN).contains(&prefix.len())
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(&b));
        if !valid {
            return Err(ParseError::BadKeyPrefix(prefix.to_owned()));
        }
        return Ok(Selector::Key(prefix.to_ascii_lowercase()));
    }
    if let Some(tag) = raw.strip_prefix('#') {
        if tag.is_empty() {
            return Err(ParseError::EmptyTag);
        }
        return Ok(Selector::Tag(tag.to_owned()));
    }

    // `name`, `name:group`, or `name:group.pane`.
    let Some((name, locus)) = raw.split_once(':') else {
        return Ok(Selector::Session(raw.to_owned()));
    };
    let name = name.to_owned();

    // The first `.` separates the group locus from an optional pane index.
    if let Some((group_part, pane_part)) = locus.split_once('.') {
        let group = parse_group_ref(group_part);
        let pane = pane_part
            .parse::<u16>()
            .map_err(|_| ParseError::BadPaneIndex(pane_part.to_owned()))?;
        Ok(Selector::Pane(name, group, pane))
    } else {
        Ok(Selector::Group(name, parse_group_ref(locus)))
    }
}

/// A group locus is a [`GroupRef::Index`] when fully numeric, else a
/// [`GroupRef::Tag`].
fn parse_group_ref(part: &str) -> GroupRef {
    part.parse::<u16>()
        .map_or_else(|_| GroupRef::Tag(part.to_owned()), GroupRef::Index)
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

#[cfg(test)]
mod tests;
