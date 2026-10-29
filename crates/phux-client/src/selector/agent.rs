use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::SessionSnapshot;

use crate::agent_meta::{AgentMetaState, AgentRecord};

use super::is_addressable_agent_name;
use super::resolve::format_terminal_id;

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

    /// The stable `--json` error code (`docs/scripting.md#json-output-and-errors`) every
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
