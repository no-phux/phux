//! The resource-kind catalog and the closed verb classification
//! ([ADR-0125], `docs/spec/workload-auth.md` §5-§6, `docs/spec/L1.md` §1.1).
//!
//! One table serves two readers. Discovery (`phux --capabilities --json` and
//! the generated `docs/reference/kinds.md`) reads [`SERVER_METHODS`],
//! [`SUBSTRATE_METHODS`], and [`KINDS`] to say what each resource kind
//! answers. Authorization reads [`classify_frame`] and [`classify_command`],
//! which return rows of [`FRAME_RULES`] and [`COMMAND_RULES`]: the same rows
//! the method entries point at, so the two readers cannot disagree about the
//! verbs a method needs.
//!
//! The catalog is compiled metadata, never wire. Invocation stays the typed
//! [`Command`] and [`FrameKind`] enums; nothing here is negotiated, and a
//! method name is not an invocation handle. Discovery is not authorization:
//! reading this table grants nothing.
//!
//! Both classifiers match their enum exhaustively, so a new frame or command
//! variant does not compile until it is classified. Whatever the tables do not
//! name (an unknown, retired, unallocated, or wrong-direction frame or tag) is
//! [`Classification::Deny`].
//!
//! [ADR-0125]: https://github.com/no-phux/phux/blob/main/docs/adr/0125-kind-catalog-is-generated-metadata.md

use crate::caps::ServerFeature;
use crate::ids::{ResourceId, ResourceKind, SatelliteHost};
use crate::wire::frame::{
    AttachTarget, COMMAND_TAG_ACQUIRE_INPUT, COMMAND_TAG_APPEND_RESOURCE_OUTPUT,
    COMMAND_TAG_APPLY_INPUT, COMMAND_TAG_ATTACH_RESOURCE, COMMAND_TAG_DETACH_CLIENTS,
    COMMAND_TAG_DETACH_RESOURCE, COMMAND_TAG_GET_PERF, COMMAND_TAG_GET_SCREEN,
    COMMAND_TAG_GET_STATE, COMMAND_TAG_GET_TERMINAL_STATE, COMMAND_TAG_KILL_RESOURCE,
    COMMAND_TAG_KILL_RESOURCE_IF, COMMAND_TAG_KILL_RESOURCES, COMMAND_TAG_OPEN_LISTENER,
    COMMAND_TAG_PUT_FILE, COMMAND_TAG_RELEASE_INPUT, COMMAND_TAG_REPORT_AGENT_STATE,
    COMMAND_TAG_REPORT_ASKED, COMMAND_TAG_ROUTE_INPUT, COMMAND_TAG_SHUTDOWN,
    COMMAND_TAG_SIGNAL_TERMINAL, COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS, COMMAND_TAG_TRANSCRIBE,
    COMMAND_TAG_UPGRADE, CONFIG_RELOAD_KEY, Command, EVENT_TAG_ASKED, EVENT_TAG_BELL,
    EVENT_TAG_COMMAND_FINISHED, EVENT_TAG_COMMAND_STARTED, EVENT_TAG_CWD_CHANGED, EVENT_TAG_DIRTY,
    EVENT_TAG_IDLE, EVENT_TAG_JOURNAL_GAP, EVENT_TAG_RESOURCE_CLOSED, EVENT_TAG_RESOURCE_SPAWNED,
    EVENT_TAG_TERMINAL_CONTROL, EVENT_TAG_TITLE_CHANGED, FrameKind, RESOURCE_AGENT_KEY,
    RESOURCE_AGENT_SESSION_KEY, RESOURCE_LINK_KEY, RESOURCE_PANE_OCCUPANT_KEY, RESOURCE_TAGS_KEY,
    SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY, SESSION_CREATE_RESULT_KEY_PREFIX,
    SESSION_KEEP_EMPTY_KEY, Scope, SpawnResource, StateScope, TYPE_ATTACH, TYPE_COMMAND,
    TYPE_DELETE_METADATA, TYPE_DETACH, TYPE_FRAME_ACK, TYPE_GET_METADATA, TYPE_HELLO,
    TYPE_HISTORY_REQUEST, TYPE_INPUT_FOCUS, TYPE_INPUT_KEY, TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE,
    TYPE_INPUT_TERMINAL_REPLY, TYPE_LIST_DIRECTORY, TYPE_LIST_METADATA, TYPE_MOVE_RESOURCE,
    TYPE_PING, TYPE_RESIZE_TERMINAL, TYPE_SET_METADATA, TYPE_SPAWN_RESOURCE, TYPE_SUBSCRIBE_EVENTS,
    TYPE_SUBSCRIBE_METADATA, TYPE_VIEWPORT_RESIZE, WHOAMI_KEY, decode_session_keep_empty,
};
use crate::wire::frame::{EVENT_TAG_SOURCE_GAP, SESSION_NAME_KEY};

// -----------------------------------------------------------------------------
// Verbs, subjects, and classifications.
// -----------------------------------------------------------------------------

/// One closed verb of `docs/spec/workload-auth.md` §5. The discriminant is the
/// verb's bit in a scope grant, byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Verb {
    /// Enumerate identities and bounded non-content state.
    Inventory = 0x01,
    /// Read content, history, events, metadata, or telemetry.
    Observe = 0x02,
    /// Create a Terminal or other endpoint resource.
    Create = 0x04,
    /// Attach, resize, move, lease, or mutate metadata or projection bindings.
    Bind = 0x08,
    /// Deliver user or terminal input, or upload workload bytes.
    Input = 0x10,
    /// Process or server lifecycle, hooks, forced detach, or signals.
    Signal = 0x20,
}

impl Verb {
    /// Every verb, in bit order.
    pub const ALL: [Self; 6] = [
        Self::Inventory,
        Self::Observe,
        Self::Create,
        Self::Bind,
        Self::Input,
        Self::Signal,
    ];

    /// The verb's bit in a scope grant.
    #[must_use]
    pub const fn bit(self) -> u8 {
        self as u8
    }

    /// The spec's name for the verb, e.g. `"OBSERVE"`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inventory => "INVENTORY",
            Self::Observe => "OBSERVE",
            Self::Create => "CREATE",
            Self::Bind => "BIND",
            Self::Input => "INPUT",
            Self::Signal => "SIGNAL",
        }
    }
}

/// A set of [`Verb`]s: the bitset a scope grant carries (workload-auth §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Verbs(u8);

impl Verbs {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// The bits a v1 grant may carry. `0xC0` is unknown in v1 and a grant
    /// carrying it is rejected, never ignored.
    pub const KNOWN_BITS: u8 = 0x3F;

    /// The verbs `INVENTORY` and `OBSERVE`: the ones that read without
    /// changing anything.
    const READ_ONLY: u8 = Verb::Inventory.bit() | Verb::Observe.bit();

    /// The set of `verbs`.
    #[must_use]
    pub const fn of(verbs: &[Verb]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < verbs.len() {
            bits |= verbs[i].bit();
            i += 1;
        }
        Self(bits)
    }

    /// The set as grant bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether `verb` is in the set.
    #[must_use]
    pub const fn contains(self, verb: Verb) -> bool {
        self.0 & verb.bit() != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the set holds any verb beyond `INVENTORY` and `OBSERVE`: an
    /// operation needing it can change server state.
    #[must_use]
    pub const fn mutates(self) -> bool {
        self.0 & !Self::READ_ONLY != 0
    }

    /// The verbs in the set, in bit order.
    pub fn iter(self) -> impl Iterator<Item = Verb> {
        Verb::ALL
            .into_iter()
            .filter(move |verb| self.contains(*verb))
    }
}

/// Why a row needs no grant at all (workload-auth §6 `*-exempt` rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exemption {
    /// `HELLO`: valid only before the handshake completes.
    Handshake,
    /// `PING`: no state access; allowed before `HELLO`.
    Liveness,
    /// `DETACH` / `DETACH_RESOURCE`: tears down the caller's own bindings.
    Cleanup,
    /// `GET_METADATA { Global, "phux.whoami/v1" }`: reads the calling
    /// connection's own identity and grant, and nothing else.
    SelfRead,
}

impl Exemption {
    /// The spec's word for the exemption, e.g. `"handshake"`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Liveness => "liveness",
            Self::Cleanup => "cleanup",
            Self::SelfRead => "self",
        }
    }
}

/// Whose authority a classified frame or command needs.
///
/// This is the subject-selector column of workload-auth §6 as a closed
/// vocabulary. A dispatch guard resolves the subject against server state,
/// side-effect-free, before any routing or handler runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
    /// No subject: an exempt handshake or liveness row, or a denied row.
    None,
    /// The calling connection's own bindings only.
    CallingConnection,
    /// The Terminal the frame or command names. A child resource matches
    /// when its parent matches (workload-auth §6, L1 §1.2).
    NamedTerminal,
    /// Every Terminal the command names, all-or-nothing.
    EveryNamedTerminal,
    /// Every Terminal the connection is attached to; zero targets is a no-op.
    AttachedTerminals,
    /// Both the moved Terminal and the destination-owner Terminal.
    MovedAndOwnerTerminals,
    /// Every Terminal the connection may observe, as a filtered
    /// subscription; server-global events require the Global selector.
    ObservableTerminals,
    /// The resources the caller's `INVENTORY` grants match; server-global
    /// data only with the Global selector.
    InventoryMatches,
    /// The resolved Group: an attach target or a forced-detach session.
    ResolvedGroup,
    /// The selected local Group a create-if-missing attach would create in.
    SelectedLocalGroup,
    /// The Group the spawn payload names.
    PayloadGroup,
    /// The satellite Host a spawn routes to.
    SatelliteHost,
    /// `CREATE` on the owner Terminal's resolved Group and `BIND` on the
    /// owner Terminal; the payload Group must equal the resolved Group.
    OwnerTerminalGroup,
    /// `CREATE` on the parent Terminal's resolved Group and `BIND` on the
    /// parent Terminal; the payload Group must equal the resolved Group.
    ParentTerminalGroup,
    /// `CREATE` on the satellite Host and `BIND` on the satellite-tagged
    /// parent Terminal.
    SatelliteHostAndParent,
    /// The named resource's parent Terminal. A grant naming only the child
    /// does not suffice.
    ParentOfNamed,
    /// The session a `phux.session.keep_empty/v1` value names, resolved
    /// side-effect-free by name.
    NamedSession,
    /// The encoded metadata [`Scope`].
    MetadataScope,
    /// The Global selector.
    Global {
        /// The transport predicate: the authenticated transport must also be
        /// the owner Unix socket, so no remote grant reaches the operation.
        owner_uds_only: bool,
    },
}

impl Subject {
    /// Whether the subject carries the owner-UDS transport predicate.
    #[must_use]
    pub const fn requires_owner_uds(self) -> bool {
        matches!(
            self,
            Self::Global {
                owner_uds_only: true
            }
        )
    }
}

/// What a row of the classification tables requires, before its subject is
/// resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Requirement {
    /// Every verb in the set, on the row's subject.
    Verbs(Verbs),
    /// No grant.
    Exempt(Exemption),
    /// The `COMMAND` envelope: the nested command's row decides, and the
    /// envelope alone grants nothing.
    Nested,
    /// Default-deny.
    Deny,
}

/// The outcome of classifying one decoded client frame or command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Classification {
    /// Admit when an effective grant carries every verb on the subject.
    Allow {
        /// The verbs the grant must carry.
        verbs: Verbs,
        /// Whom the grant must cover.
        subject: Subject,
    },
    /// Admit without a grant.
    Exempt(Exemption),
    /// Refuse.
    Deny,
}

/// One row of a workload-auth §6 classification table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rule {
    /// The row's first cell, verbatim from the spec: the case it covers.
    pub case: &'static str,
    /// What the row requires.
    pub requirement: Requirement,
    /// Whom the requirement applies to.
    pub subject: Subject,
}

impl Rule {
    const fn verbs(case: &'static str, verbs: &[Verb], subject: Subject) -> Self {
        Self {
            case,
            requirement: Requirement::Verbs(Verbs::of(verbs)),
            subject,
        }
    }

    const fn exempt(case: &'static str, exemption: Exemption, subject: Subject) -> Self {
        Self {
            case,
            requirement: Requirement::Exempt(exemption),
            subject,
        }
    }

    const fn deny(case: &'static str) -> Self {
        Self {
            case,
            requirement: Requirement::Deny,
            subject: Subject::None,
        }
    }

    /// The row as a classification. The `COMMAND` envelope row is `Deny`:
    /// only its nested command's row can admit anything.
    #[must_use]
    pub const fn classification(&self) -> Classification {
        match self.requirement {
            Requirement::Verbs(verbs) => Classification::Allow {
                verbs,
                subject: self.subject,
            },
            Requirement::Exempt(exemption) => Classification::Exempt(exemption),
            Requirement::Nested | Requirement::Deny => Classification::Deny,
        }
    }

    /// The verbs the row requires; empty for an exempt, nested, or denied row.
    #[must_use]
    pub const fn verb_set(&self) -> Verbs {
        match self.requirement {
            Requirement::Verbs(verbs) => verbs,
            Requirement::Exempt(_) | Requirement::Nested | Requirement::Deny => Verbs::EMPTY,
        }
    }

    /// A canonical one-line label for the requirement: `BIND+OBSERVE` (verbs
    /// in bit order), `SIGNAL + owner-UDS transport`, `exempt: cleanup`,
    /// `nested`, or `deny`. The spec golden and the generated reference both
    /// render requirements through this.
    #[must_use]
    pub fn requirement_label(&self) -> String {
        match self.requirement {
            Requirement::Verbs(verbs) => verbs_label(verbs, self.subject),
            Requirement::Exempt(exemption) => format!("exempt: {}", exemption.name()),
            Requirement::Nested => "nested".to_owned(),
            Requirement::Deny => "deny".to_owned(),
        }
    }
}

fn verbs_label(verbs: Verbs, subject: Subject) -> String {
    let mut label = verbs.iter().map(Verb::name).collect::<Vec<_>>().join("+");
    if subject.requires_owner_uds() {
        label.push_str(" + owner-UDS transport");
    }
    label
}

// -----------------------------------------------------------------------------
// The client-frame table (workload-auth §6, first table), in spec row order.
// -----------------------------------------------------------------------------

static F_HELLO: Rule = Rule::exempt("`HELLO`", Exemption::Handshake, Subject::None);
static F_PING: Rule = Rule::exempt("`PING`", Exemption::Liveness, Subject::None);
static F_DETACH: Rule = Rule::exempt("`DETACH`", Exemption::Cleanup, Subject::CallingConnection);
static F_ATTACH: Rule = Rule::verbs(
    "`ATTACH` existing/last target",
    &[Verb::Bind, Verb::Observe],
    Subject::ResolvedGroup,
);
static F_ATTACH_CREATE: Rule = Rule::verbs(
    "`ATTACH` create-if-missing",
    &[Verb::Create, Verb::Bind, Verb::Observe],
    Subject::SelectedLocalGroup,
);
static F_HISTORY_REQUEST: Rule = Rule::verbs(
    "`HISTORY_REQUEST`",
    &[Verb::Observe],
    Subject::NamedTerminal,
);
static F_FRAME_ACK: Rule = Rule::verbs("`FRAME_ACK`", &[Verb::Observe], Subject::NamedTerminal);
static F_COMMAND: Rule = Rule {
    case: "`COMMAND`",
    requirement: Requirement::Nested,
    subject: Subject::None,
};
static F_SUBSCRIBE: Rule = Rule::deny("`SUBSCRIBE` (unallocated)");
static F_INPUT: Rule = Rule::verbs(
    "`INPUT_KEY`, `INPUT_PASTE`, `INPUT_MOUSE`, `INPUT_RAW`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY`",
    &[Verb::Input],
    Subject::NamedTerminal,
);
static F_VIEWPORT_RESIZE: Rule = Rule::verbs(
    "`VIEWPORT_RESIZE`",
    &[Verb::Bind],
    Subject::AttachedTerminals,
);
static F_SPAWN_SATELLITE: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { satellite: Some, owner_terminal: None }`",
    &[Verb::Create],
    Subject::SatelliteHost,
);
static F_SPAWN_LOCAL: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { satellite: None, owner_terminal: None }`",
    &[Verb::Create],
    Subject::PayloadGroup,
);
static F_SPAWN_OWNED: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { satellite: None, owner_terminal: Some }`",
    &[Verb::Create, Verb::Bind],
    Subject::OwnerTerminalGroup,
);
static F_SPAWN_SATELLITE_OWNED: Rule =
    Rule::deny("`SPAWN_RESOURCE { satellite: Some, owner_terminal: Some }`");
static F_SPAWN_AGENT_LOCAL: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: None, parent: Some }`",
    &[Verb::Create, Verb::Bind],
    Subject::ParentTerminalGroup,
);
static F_SPAWN_AGENT_SATELLITE: Rule = Rule::verbs(
    "`SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: Some(H), parent: Some(Satellite { H, .. }) }`",
    &[Verb::Create, Verb::Bind],
    Subject::SatelliteHostAndParent,
);
static F_SPAWN_KIND_MISMATCH: Rule = Rule::deny(
    "`SPAWN_RESOURCE { kind: not TERMINAL, parent: None }` or `{ kind: TERMINAL, parent: Some }`",
);
static F_RESIZE_TERMINAL: Rule =
    Rule::verbs("`RESIZE_TERMINAL`", &[Verb::Bind], Subject::NamedTerminal);
static F_MOVE_RESOURCE: Rule = Rule::verbs(
    "`MOVE_RESOURCE`",
    &[Verb::Bind],
    Subject::MovedAndOwnerTerminals,
);
static F_SUBSCRIBE_EVENTS_ONE: Rule = Rule::verbs(
    "`SUBSCRIBE_EVENTS { terminal: Some }`",
    &[Verb::Observe],
    Subject::NamedTerminal,
);
static F_SUBSCRIBE_EVENTS_ALL: Rule = Rule::verbs(
    "`SUBSCRIBE_EVENTS { terminal: None }`",
    &[Verb::Observe],
    Subject::ObservableTerminals,
);
static F_WHOAMI: Rule = Rule::exempt(
    r#"`GET_METADATA { Global, "phux.whoami/v1" }`"#,
    Exemption::SelfRead,
    Subject::CallingConnection,
);
static F_GET_METADATA: Rule = Rule::verbs(
    "Other `GET_METADATA`",
    &[Verb::Observe],
    Subject::MetadataScope,
);
static F_SESSION_CREATE: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.session.create/v1" }`"#,
    &[Verb::Create, Verb::Bind],
    Subject::Global {
        owner_uds_only: false,
    },
);
static F_KEEP_EMPTY_MARK: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0true`"#,
    &[Verb::Create, Verb::Bind],
    Subject::Global {
        owner_uds_only: false,
    },
);
static F_KEEP_EMPTY_CLEAR: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0false`"#,
    &[Verb::Signal],
    Subject::NamedSession,
);
static F_KEEP_EMPTY_OTHER: Rule =
    Rule::deny(r#"`SET_METADATA { Global, "phux.session.keep_empty/v1" }` with any other value"#);
static F_CONFIG_RELOAD: Rule = Rule::verbs(
    r#"`SET_METADATA { Global, "phux.config.reload/v1" }`"#,
    &[Verb::Signal],
    Subject::Global {
        owner_uds_only: false,
    },
);
static F_RESULT_NAMESPACE_WRITE: Rule = Rule::deny(
    "`SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results",
);
static F_RESULT_NAMESPACE_SUBSCRIBE: Rule =
    Rule::deny("`SUBSCRIBE_METADATA` targeting that result namespace");
static F_SERVER_OWNED_WRITE: Rule = Rule::deny(
    "`SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1` or `phux.whoami/v1`, or `DELETE_METADATA` targeting `phux.config.reload/v1` or `phux.session.keep_empty/v1`",
);
static F_METADATA_WRITE: Rule = Rule::verbs(
    "Other `SET_METADATA`, `DELETE_METADATA`",
    &[Verb::Bind],
    Subject::MetadataScope,
);
static F_LIST_METADATA: Rule = Rule::verbs(
    "`LIST_METADATA`",
    &[Verb::Inventory],
    Subject::MetadataScope,
);
static F_LIST_DIRECTORY: Rule = Rule::verbs(
    "`LIST_DIRECTORY`",
    &[Verb::Inventory],
    Subject::Global {
        owner_uds_only: false,
    },
);
static F_SUBSCRIBE_METADATA: Rule = Rule::verbs(
    "Other `SUBSCRIBE_METADATA`",
    &[Verb::Observe],
    Subject::MetadataScope,
);
static F_UNCLASSIFIED: Rule =
    Rule::deny("Unknown, wrong-direction, retired, or otherwise unclassified frame");

/// The client-frame table of workload-auth §6, in spec row order.
///
/// [`classify_frame`] returns one of these rows for every decoded frame. It
/// never returns the `COMMAND` row: the nested command decides.
pub static FRAME_RULES: [&Rule; 37] = [
    &F_HELLO,
    &F_PING,
    &F_DETACH,
    &F_ATTACH,
    &F_ATTACH_CREATE,
    &F_HISTORY_REQUEST,
    &F_FRAME_ACK,
    &F_COMMAND,
    &F_SUBSCRIBE,
    &F_INPUT,
    &F_VIEWPORT_RESIZE,
    &F_SPAWN_SATELLITE,
    &F_SPAWN_LOCAL,
    &F_SPAWN_OWNED,
    &F_SPAWN_SATELLITE_OWNED,
    &F_SPAWN_AGENT_LOCAL,
    &F_SPAWN_AGENT_SATELLITE,
    &F_SPAWN_KIND_MISMATCH,
    &F_RESIZE_TERMINAL,
    &F_MOVE_RESOURCE,
    &F_SUBSCRIBE_EVENTS_ONE,
    &F_SUBSCRIBE_EVENTS_ALL,
    &F_WHOAMI,
    &F_GET_METADATA,
    &F_SESSION_CREATE,
    &F_KEEP_EMPTY_MARK,
    &F_KEEP_EMPTY_CLEAR,
    &F_KEEP_EMPTY_OTHER,
    &F_CONFIG_RELOAD,
    &F_RESULT_NAMESPACE_WRITE,
    &F_RESULT_NAMESPACE_SUBSCRIBE,
    &F_SERVER_OWNED_WRITE,
    &F_METADATA_WRITE,
    &F_LIST_METADATA,
    &F_LIST_DIRECTORY,
    &F_SUBSCRIBE_METADATA,
    &F_UNCLASSIFIED,
];

// -----------------------------------------------------------------------------
// The nested-command table (workload-auth §6, second table), in spec order.
// -----------------------------------------------------------------------------

static C_SPAWN: Rule = Rule::deny("`SPAWN` (unallocated)");
static C_ATTACH_RESOURCE: Rule = Rule::verbs(
    "`ATTACH_RESOURCE`",
    &[Verb::Bind, Verb::Observe],
    Subject::NamedTerminal,
);
static C_DETACH_RESOURCE: Rule = Rule::exempt(
    "`DETACH_RESOURCE`",
    Exemption::Cleanup,
    Subject::CallingConnection,
);
static C_KILL_RESOURCE: Rule =
    Rule::verbs("`KILL_RESOURCE`", &[Verb::Signal], Subject::NamedTerminal);
static C_KILL_RESOURCE_IF: Rule = Rule::verbs(
    "`KILL_RESOURCE_IF`",
    &[Verb::Signal],
    Subject::NamedTerminal,
);
static C_GET_SCREEN: Rule = Rule::verbs("`GET_SCREEN`", &[Verb::Observe], Subject::NamedTerminal);
static C_INPUT: Rule = Rule::verbs(
    "`ROUTE_INPUT`, `APPLY_INPUT`",
    &[Verb::Input],
    Subject::NamedTerminal,
);
static C_KILL_RESOURCES: Rule = Rule::verbs(
    "`KILL_RESOURCES`",
    &[Verb::Signal],
    Subject::EveryNamedTerminal,
);
static C_RESIZE_TERMINAL: Rule = Rule::deny("`RESIZE_TERMINAL` (unallocated)");
static C_GET_STATE: Rule = Rule::verbs(
    "`GET_STATE { SERVER }`",
    &[Verb::Inventory],
    Subject::InventoryMatches,
);
static C_RUN_HOOK: Rule = Rule::deny("`RUN_HOOK` (unallocated)");
static C_GET_TERMINAL_STATE: Rule = Rule::verbs(
    "`GET_TERMINAL_STATE`",
    &[Verb::Inventory],
    Subject::NamedTerminal,
);
static C_SUBSCRIBE_RESOURCE_EVENTS: Rule = Rule::verbs(
    "`SUBSCRIBE_RESOURCE_EVENTS`",
    &[Verb::Observe],
    Subject::NamedTerminal,
);
static C_UPGRADE: Rule = Rule::verbs(
    "`UPGRADE`",
    &[Verb::Signal],
    Subject::Global {
        owner_uds_only: false,
    },
);
static C_INPUT_LEASE: Rule = Rule::verbs(
    "`ACQUIRE_INPUT`, `RELEASE_INPUT`",
    &[Verb::Bind],
    Subject::NamedTerminal,
);
static C_SIGNAL_TERMINAL: Rule =
    Rule::verbs("`SIGNAL_TERMINAL`", &[Verb::Signal], Subject::NamedTerminal);
static C_AGENT_REPORT: Rule = Rule::verbs(
    "`REPORT_ASKED`, `REPORT_AGENT_STATE`",
    &[Verb::Bind],
    Subject::NamedTerminal,
);
static C_PUT_FILE: Rule = Rule::verbs("`PUT_FILE`", &[Verb::Input], Subject::NamedTerminal);
static C_TRANSCRIBE: Rule = Rule::verbs("`TRANSCRIBE`", &[Verb::Input], Subject::NamedTerminal);
static C_DETACH_CLIENTS_SESSION: Rule = Rule::verbs(
    "`DETACH_CLIENTS { session: Some }`",
    &[Verb::Signal],
    Subject::ResolvedGroup,
);
static C_DETACH_CLIENTS_ALL: Rule = Rule::verbs(
    "`DETACH_CLIENTS { session: None }`",
    &[Verb::Signal],
    Subject::Global {
        owner_uds_only: false,
    },
);
static C_SHUTDOWN: Rule = Rule::verbs(
    "`SHUTDOWN`",
    &[Verb::Signal],
    Subject::Global {
        owner_uds_only: true,
    },
);
static C_OPEN_LISTENER: Rule = Rule::verbs(
    "`OPEN_LISTENER`",
    &[Verb::Signal],
    Subject::Global {
        owner_uds_only: true,
    },
);
static C_GET_PERF: Rule = Rule::verbs(
    "`GET_PERF { reset: false }`",
    &[Verb::Observe],
    Subject::Global {
        owner_uds_only: false,
    },
);
static C_GET_PERF_RESET: Rule = Rule::verbs(
    "`GET_PERF { reset: true }`",
    &[Verb::Observe, Verb::Bind],
    Subject::Global {
        owner_uds_only: false,
    },
);
static C_APPEND_RESOURCE_OUTPUT: Rule = Rule::verbs(
    "`APPEND_RESOURCE_OUTPUT`",
    &[Verb::Bind, Verb::Input],
    Subject::ParentOfNamed,
);
static C_UNCLASSIFIED: Rule = Rule::deny("Unknown, retired, or otherwise unclassified command tag");

/// The nested-command table of workload-auth §6, in spec row order.
///
/// [`classify_command`] returns one of these rows for every decoded command.
pub static COMMAND_RULES: [&Rule; 27] = [
    &C_SPAWN,
    &C_ATTACH_RESOURCE,
    &C_DETACH_RESOURCE,
    &C_KILL_RESOURCE,
    &C_KILL_RESOURCE_IF,
    &C_GET_SCREEN,
    &C_INPUT,
    &C_KILL_RESOURCES,
    &C_RESIZE_TERMINAL,
    &C_GET_STATE,
    &C_RUN_HOOK,
    &C_GET_TERMINAL_STATE,
    &C_SUBSCRIBE_RESOURCE_EVENTS,
    &C_UPGRADE,
    &C_INPUT_LEASE,
    &C_SIGNAL_TERMINAL,
    &C_AGENT_REPORT,
    &C_PUT_FILE,
    &C_TRANSCRIBE,
    &C_DETACH_CLIENTS_SESSION,
    &C_DETACH_CLIENTS_ALL,
    &C_SHUTDOWN,
    &C_OPEN_LISTENER,
    &C_GET_PERF,
    &C_GET_PERF_RESET,
    &C_APPEND_RESOURCE_OUTPUT,
    &C_UNCLASSIFIED,
];

// -----------------------------------------------------------------------------
// Classifiers.
// -----------------------------------------------------------------------------

/// Classify one decoded client frame (workload-auth §6).
///
/// A `COMMAND` frame is classified by its nested command; a server-to-client
/// frame arriving from a client is [`Classification::Deny`].
#[must_use]
pub fn classify_frame(frame: &FrameKind) -> Classification {
    frame_rule(frame).classification()
}

/// Classify one decoded nested command (workload-auth §6).
#[must_use]
pub fn classify_command(command: &Command) -> Classification {
    command_rule(command).classification()
}

/// The workload-auth §6 row that classifies `frame`.
///
/// The match is exhaustive: a new [`FrameKind`] variant does not compile
/// until it is placed here. A `COMMAND` frame returns its nested command's
/// row from [`command_rule`]; a server-to-client frame returns the default-deny
/// row.
#[must_use]
pub fn frame_rule(frame: &FrameKind) -> &'static Rule {
    match frame {
        FrameKind::Hello { .. } => &F_HELLO,
        FrameKind::Ping { .. } => &F_PING,
        FrameKind::Detach => &F_DETACH,
        FrameKind::Attach { target, .. } => attach_rule(target),
        FrameKind::HistoryRequest { .. } => &F_HISTORY_REQUEST,
        FrameKind::FrameAck { .. } => &F_FRAME_ACK,
        FrameKind::Command { command, .. } => command_rule(command),
        FrameKind::InputKey { .. }
        | FrameKind::InputMouse { .. }
        | FrameKind::InputFocus { .. }
        | FrameKind::InputPaste { .. }
        | FrameKind::InputTerminalReply { .. } => &F_INPUT,
        FrameKind::ViewportResize { .. } => &F_VIEWPORT_RESIZE,
        FrameKind::SpawnResource {
            satellite,
            owner_terminal,
            resource,
            ..
        } => spawn_rule(
            satellite.as_ref(),
            owner_terminal.is_some(),
            resource.as_deref(),
        ),
        FrameKind::ResizeTerminal { .. } => &F_RESIZE_TERMINAL,
        FrameKind::MoveResource { .. } => &F_MOVE_RESOURCE,
        FrameKind::SubscribeEvents {
            terminal: Some(_), ..
        } => &F_SUBSCRIBE_EVENTS_ONE,
        FrameKind::SubscribeEvents { terminal: None, .. } => &F_SUBSCRIBE_EVENTS_ALL,
        FrameKind::GetMetadata { scope, key, .. } => get_metadata_rule(scope, key),
        FrameKind::SetMetadata {
            scope, key, value, ..
        } => set_metadata_rule(scope, key, value),
        FrameKind::DeleteMetadata { key, .. } => delete_metadata_rule(key),
        FrameKind::ListMetadata { .. } => &F_LIST_METADATA,
        FrameKind::ListDirectory { .. } => &F_LIST_DIRECTORY,
        FrameKind::SubscribeMetadata { key, .. } => subscribe_metadata_rule(key),
        // Server-to-client frames: wrong direction when a client sends them.
        FrameKind::HelloOk { .. }
        | FrameKind::Pong { .. }
        | FrameKind::ResourceOutput { .. }
        | FrameKind::Attached { .. }
        | FrameKind::AttachReady { .. }
        | FrameKind::Detached { .. }
        | FrameKind::BootstrapBegin { .. }
        | FrameKind::BootstrapChunk { .. }
        | FrameKind::BootstrapReady { .. }
        | FrameKind::HistoryPage { .. }
        | FrameKind::BootstrapTombstone { .. }
        | FrameKind::HistoryTombstone { .. }
        | FrameKind::HistoryRejected { .. }
        | FrameKind::Bell { .. }
        | FrameKind::Error { .. }
        | FrameKind::MetadataChanged { .. }
        | FrameKind::MetadataValue { .. }
        | FrameKind::MetadataKeys { .. }
        | FrameKind::DirectoryListing { .. }
        | FrameKind::ResourceSpawned { .. }
        | FrameKind::ResourceMoved { .. }
        | FrameKind::ResourceClosed { .. }
        | FrameKind::CommandResult { .. }
        | FrameKind::Event { .. } => &F_UNCLASSIFIED,
    }
}

/// The workload-auth §6 row that classifies the nested `command`.
///
/// The match is exhaustive: a new [`Command`] variant does not compile until
/// it is placed here, and a command the spec's table does not list is placed
/// on the default-deny row until the spec classifies it.
#[must_use]
pub fn command_rule(command: &Command) -> &'static Rule {
    match command {
        Command::AttachResource { .. } => &C_ATTACH_RESOURCE,
        Command::DetachResource { .. } => &C_DETACH_RESOURCE,
        Command::KillResource { .. } => &C_KILL_RESOURCE,
        Command::KillResourceIf { .. } => &C_KILL_RESOURCE_IF,
        Command::GetScreen { .. } => &C_GET_SCREEN,
        Command::RouteInput { .. } | Command::ApplyInput { .. } => &C_INPUT,
        Command::KillResources { .. } => &C_KILL_RESOURCES,
        Command::GetState {
            scope: StateScope::Server,
        } => &C_GET_STATE,
        Command::GetTerminalState { .. } => &C_GET_TERMINAL_STATE,
        Command::SubscribeResourceEvents { .. } => &C_SUBSCRIBE_RESOURCE_EVENTS,
        Command::Upgrade => &C_UPGRADE,
        Command::AcquireInput { .. } | Command::ReleaseInput { .. } => &C_INPUT_LEASE,
        Command::SignalTerminal { .. } => &C_SIGNAL_TERMINAL,
        Command::ReportAsked { .. } | Command::ReportAgentState { .. } => &C_AGENT_REPORT,
        Command::PutFile { .. } => &C_PUT_FILE,
        Command::DetachClients { session: Some(_) } => &C_DETACH_CLIENTS_SESSION,
        Command::DetachClients { session: None } => &C_DETACH_CLIENTS_ALL,
        Command::Shutdown => &C_SHUTDOWN,
        Command::OpenListener { .. } => &C_OPEN_LISTENER,
        Command::GetPerf { reset: false } => &C_GET_PERF,
        Command::GetPerf { reset: true } => &C_GET_PERF_RESET,
        Command::AppendResourceOutput { .. } => &C_APPEND_RESOURCE_OUTPUT,
        Command::Transcribe { .. } => &C_TRANSCRIBE,
    }
}

fn attach_rule(target: &AttachTarget) -> &'static Rule {
    match target {
        AttachTarget::CreateIfMissing { .. } => &F_ATTACH_CREATE,
        AttachTarget::Last | AttachTarget::ByName(_) | AttachTarget::ById(_) => &F_ATTACH,
    }
}

/// The spawn rows. A kind that contradicts its binding is refused first, so
/// the decoder's `SPAWN_FAILED` never runs; a Terminal is then placed by its
/// satellite and owner fields, and an agent session by its parent.
fn spawn_rule(
    satellite: Option<&SatelliteHost>,
    owned: bool,
    resource: Option<&SpawnResource>,
) -> &'static Rule {
    let kind = resource.map_or(ResourceKind::Terminal, |spawn| spawn.kind);
    let parent = resource.and_then(|spawn| spawn.parent.as_ref());
    if kind_contradicts_binding(kind, parent) {
        return &F_SPAWN_KIND_MISMATCH;
    }
    match parent {
        None => terminal_spawn_rule(satellite.is_some(), owned),
        Some(parent) if is_unowned_agent_session(kind, owned) => {
            agent_session_spawn_rule(satellite, parent)
        }
        // An unknown child kind, or an agent session that also names an
        // owner Terminal: no row covers it.
        Some(_) => &F_UNCLASSIFIED,
    }
}

/// A Terminal has no parent and every other kind has one (L1 §1.2).
const fn kind_contradicts_binding(kind: ResourceKind, parent: Option<&ResourceId>) -> bool {
    kind.is_terminal() == parent.is_some()
}

const fn is_unowned_agent_session(kind: ResourceKind, owned: bool) -> bool {
    matches!(kind, ResourceKind::AgentSession) && !owned
}

fn terminal_spawn_rule(remote: bool, owned: bool) -> &'static Rule {
    match (remote, owned) {
        (false, false) => &F_SPAWN_LOCAL,
        (false, true) => &F_SPAWN_OWNED,
        (true, false) => &F_SPAWN_SATELLITE,
        (true, true) => &F_SPAWN_SATELLITE_OWNED,
    }
}

/// A remote agent session must name a parent on the same satellite; a local
/// or different-host parent is default-deny.
fn agent_session_spawn_rule(
    satellite: Option<&SatelliteHost>,
    parent: &ResourceId,
) -> &'static Rule {
    let Some(host) = satellite else {
        return &F_SPAWN_AGENT_LOCAL;
    };
    if parent_lives_on(parent, host) {
        &F_SPAWN_AGENT_SATELLITE
    } else {
        &F_UNCLASSIFIED
    }
}

fn parent_lives_on(parent: &ResourceId, host: &SatelliteHost) -> bool {
    matches!(parent, ResourceId::Satellite { host: parent_host, .. } if parent_host == host)
}

/// `phux.session.created/v1` and its slash-prefixed one-shot results: the
/// server-owned, connection-private result namespace.
fn is_session_create_result(key: &str) -> bool {
    key == SESSION_CREATE_RESULT_KEY || key.starts_with(SESSION_CREATE_RESULT_KEY_PREFIX)
}

/// Keys the server owns and no client may set or delete.
fn is_read_only_server_key(key: &str) -> bool {
    key == RESOURCE_PANE_OCCUPANT_KEY || key == WHOAMI_KEY
}

/// Keys the server applies on write, which are therefore never deletable.
fn is_server_applied_key(key: &str) -> bool {
    key == CONFIG_RELOAD_KEY || key == SESSION_KEEP_EMPTY_KEY
}

fn set_metadata_rule(scope: &Scope, key: &str, value: &[u8]) -> &'static Rule {
    if is_session_create_result(key) {
        return &F_RESULT_NAMESPACE_WRITE;
    }
    if is_read_only_server_key(key) {
        return &F_SERVER_OWNED_WRITE;
    }
    if matches!(scope, Scope::Global) {
        global_set_metadata_rule(key, value)
    } else {
        &F_METADATA_WRITE
    }
}

fn global_set_metadata_rule(key: &str, value: &[u8]) -> &'static Rule {
    match key {
        SESSION_CREATE_KEY => &F_SESSION_CREATE,
        SESSION_KEEP_EMPTY_KEY => keep_empty_rule(value),
        CONFIG_RELOAD_KEY => &F_CONFIG_RELOAD,
        _ => &F_METADATA_WRITE,
    }
}

/// Setting the mark (`name\0true`) and clearing it (`name\0false`) have rows
/// of their own; any other value is malformed. The value is classified
/// before any handler parses it.
fn keep_empty_rule(value: &[u8]) -> &'static Rule {
    match decode_session_keep_empty(value) {
        Some((_, true)) => &F_KEEP_EMPTY_MARK,
        Some((_, false)) => &F_KEEP_EMPTY_CLEAR,
        None => &F_KEEP_EMPTY_OTHER,
    }
}

/// Reading `phux.whoami/v1` at Global answers only the caller's own identity
/// and grant, so it needs no verb; every other read needs `OBSERVE`.
fn get_metadata_rule(scope: &Scope, key: &str) -> &'static Rule {
    if matches!(scope, Scope::Global) && key == WHOAMI_KEY {
        &F_WHOAMI
    } else {
        &F_GET_METADATA
    }
}

fn delete_metadata_rule(key: &str) -> &'static Rule {
    if is_session_create_result(key) {
        return &F_RESULT_NAMESPACE_WRITE;
    }
    if is_read_only_server_key(key) || is_server_applied_key(key) {
        return &F_SERVER_OWNED_WRITE;
    }
    &F_METADATA_WRITE
}

fn subscribe_metadata_rule(key: &str) -> &'static Rule {
    if is_session_create_result(key) {
        &F_RESULT_NAMESPACE_SUBSCRIBE
    } else {
        &F_SUBSCRIBE_METADATA
    }
}

// -----------------------------------------------------------------------------
// The catalog.
// -----------------------------------------------------------------------------

/// How a method reaches the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Carrier {
    /// A client-to-server frame, by type byte.
    Frame(u8),
    /// A command inside the `COMMAND` envelope, by nested tag.
    Command(u8),
    /// A conventional Global L3 metadata key whose writes or reads have a
    /// catalog entry of their own, carried by the metadata frames: a key the
    /// server intercepts (session create, rename, keep-empty) or answers
    /// (whoami), or a consumer doorbell the server stores like any value
    /// (config reload).
    Metadata(&'static str),
}

/// One catalog method: a client-originated frame, a nested command, or a
/// server-interpreted metadata key.
#[derive(Debug, Clone, Copy)]
pub struct MethodSpec {
    /// The wire name (`GET_SCREEN`), or the key for a metadata carrier.
    pub name: &'static str,
    /// How the method reaches the server.
    pub carrier: Carrier,
    /// Every classification row an instance of the method can land on; the
    /// payload picks one.
    pub rules: &'static [&'static Rule],
    /// The feature bit a client must see in `HELLO_OK` before relying on the
    /// method, if any.
    pub gate: Option<ServerFeature>,
    /// Whether the codec and reference server implement it. `false` marks a
    /// spec-only allocation.
    pub shipped: bool,
}

impl MethodSpec {
    /// Every verb any of the method's rows can require.
    #[must_use]
    pub fn verbs(&self) -> Verbs {
        self.rules
            .iter()
            .fold(Verbs::EMPTY, |verbs, rule| verbs.union(rule.verb_set()))
    }

    /// Whether some instance of the method can change server state.
    ///
    /// Conservative: a method no row admits by verb (the `COMMAND`
    /// envelope, or a method every row denies) is reported as mutating, so
    /// a read-only hint derived from this can never cover a denied write.
    /// Only a method whose rows are all exemptions, or whose admitted verbs
    /// are `INVENTORY` and `OBSERVE` alone, is read-only.
    #[must_use]
    pub fn mutating(&self) -> bool {
        let verbs = self.verbs();
        if verbs.is_empty() {
            return !self.is_exempt_only();
        }
        verbs.mutates()
    }

    /// Whether every row is an exemption (handshake, liveness, cleanup).
    fn is_exempt_only(&self) -> bool {
        self.rules
            .iter()
            .all(|rule| matches!(rule.requirement, Requirement::Exempt(_)))
    }

    /// Whether the method is reachable only over the owner Unix socket.
    #[must_use]
    pub fn owner_uds_only(&self) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.subject.requires_owner_uds())
    }
}

/// One `AgentEvent` variant (L1 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventSpec {
    /// The spec's name for the event, e.g. `cwd_changed`.
    pub name: &'static str,
    /// The event's wire tag inside `EVENT`.
    pub tag: u8,
}

/// One resource kind and its facet (L1 §1.1): the methods, events, and
/// resource-scoped metadata keys only that kind answers.
#[derive(Debug, Clone, Copy)]
pub struct KindSpec {
    /// The kind.
    pub kind: ResourceKind,
    /// The spec's name for the kind, e.g. `TERMINAL`.
    pub name: &'static str,
    /// The feature bit that says the server serves this kind, if any.
    pub gate: Option<ServerFeature>,
    /// The facet methods.
    pub methods: &'static [MethodSpec],
    /// The facet events.
    pub events: &'static [EventSpec],
    /// Conventional metadata keys scoped to a resource of this kind.
    pub metadata_keys: &'static [&'static str],
}

/// `INPUT_RAW`'s frame type: allocated in L1 §2 but spec-only, so the codec
/// declares no constant for it.
const INPUT_RAW_RESERVED_TYPE: u8 = 0x13;

macro_rules! method {
    ($name:expr, $carrier:expr, [$($rule:ident),+ $(,)?], $gate:expr) => {
        MethodSpec {
            name: $name,
            carrier: $carrier,
            rules: &[$(&$rule),+],
            gate: $gate,
            shipped: true,
        }
    };
    ($name:expr, $carrier:expr, [$($rule:ident),+ $(,)?]) => {
        method!($name, $carrier, [$($rule),+], None)
    };
}

/// Methods addressed to the server or the connection, not to one resource.
///
/// These are the handshake, session attach, server lifecycle, telemetry,
/// and the Global metadata keys with entries of their own
/// ([`Carrier::Metadata`]).
pub static SERVER_METHODS: &[MethodSpec] = &[
    method!("HELLO", Carrier::Frame(TYPE_HELLO), [F_HELLO]),
    method!("PING", Carrier::Frame(TYPE_PING), [F_PING]),
    method!(
        "ATTACH",
        Carrier::Frame(TYPE_ATTACH),
        [F_ATTACH, F_ATTACH_CREATE]
    ),
    method!("DETACH", Carrier::Frame(TYPE_DETACH), [F_DETACH]),
    method!(
        "VIEWPORT_RESIZE",
        Carrier::Frame(TYPE_VIEWPORT_RESIZE),
        [F_VIEWPORT_RESIZE]
    ),
    method!("COMMAND", Carrier::Frame(TYPE_COMMAND), [F_COMMAND]),
    method!(
        "LIST_DIRECTORY",
        Carrier::Frame(TYPE_LIST_DIRECTORY),
        [F_LIST_DIRECTORY],
        Some(ServerFeature::ListDirectory)
    ),
    method!(
        "UPGRADE",
        Carrier::Command(COMMAND_TAG_UPGRADE),
        [C_UPGRADE]
    ),
    method!(
        "SHUTDOWN",
        Carrier::Command(COMMAND_TAG_SHUTDOWN),
        [C_SHUTDOWN],
        Some(ServerFeature::Shutdown)
    ),
    method!(
        "OPEN_LISTENER",
        Carrier::Command(COMMAND_TAG_OPEN_LISTENER),
        [C_OPEN_LISTENER],
        Some(ServerFeature::OpenListener)
    ),
    method!(
        "DETACH_CLIENTS",
        Carrier::Command(COMMAND_TAG_DETACH_CLIENTS),
        [C_DETACH_CLIENTS_SESSION, C_DETACH_CLIENTS_ALL]
    ),
    method!(
        "GET_PERF",
        Carrier::Command(COMMAND_TAG_GET_PERF),
        [C_GET_PERF, C_GET_PERF_RESET],
        Some(ServerFeature::GetPerf)
    ),
    method!(
        SESSION_CREATE_KEY,
        Carrier::Metadata(SESSION_CREATE_KEY),
        [F_SESSION_CREATE]
    ),
    // Session rename is server-intercepted, but §6 gives it no row of its
    // own: a write lands on the ordinary metadata-write row (BIND on Global).
    method!(
        SESSION_NAME_KEY,
        Carrier::Metadata(SESSION_NAME_KEY),
        [F_METADATA_WRITE]
    ),
    method!(
        SESSION_KEEP_EMPTY_KEY,
        Carrier::Metadata(SESSION_KEEP_EMPTY_KEY),
        [F_KEEP_EMPTY_MARK, F_KEEP_EMPTY_CLEAR, F_KEEP_EMPTY_OTHER],
        Some(ServerFeature::KeepEmptySessions)
    ),
    // A TUI doorbell: the server stores the nonce like any value, and §6
    // classifies the write as SIGNAL because it makes consumers reload.
    method!(
        CONFIG_RELOAD_KEY,
        Carrier::Metadata(CONFIG_RELOAD_KEY),
        [F_CONFIG_RELOAD]
    ),
    method!(
        WHOAMI_KEY,
        Carrier::Metadata(WHOAMI_KEY),
        [F_WHOAMI],
        Some(ServerFeature::Whoami)
    ),
];

/// The substrate every kind shares (L1 §1.1): spawn and close, the output
/// subscription, kill, events, inventory, and the resource's L3 scope.
pub static SUBSTRATE_METHODS: &[MethodSpec] = &[
    method!(
        "SPAWN_RESOURCE",
        Carrier::Frame(TYPE_SPAWN_RESOURCE),
        [
            F_SPAWN_SATELLITE,
            F_SPAWN_LOCAL,
            F_SPAWN_OWNED,
            F_SPAWN_SATELLITE_OWNED,
            F_SPAWN_AGENT_LOCAL,
            F_SPAWN_AGENT_SATELLITE,
            F_SPAWN_KIND_MISMATCH,
            F_UNCLASSIFIED,
        ]
    ),
    method!(
        "ATTACH_RESOURCE",
        Carrier::Command(COMMAND_TAG_ATTACH_RESOURCE),
        [C_ATTACH_RESOURCE]
    ),
    method!(
        "DETACH_RESOURCE",
        Carrier::Command(COMMAND_TAG_DETACH_RESOURCE),
        [C_DETACH_RESOURCE]
    ),
    method!(
        "KILL_RESOURCE",
        Carrier::Command(COMMAND_TAG_KILL_RESOURCE),
        [C_KILL_RESOURCE]
    ),
    method!(
        "KILL_RESOURCE_IF",
        Carrier::Command(COMMAND_TAG_KILL_RESOURCE_IF),
        [C_KILL_RESOURCE_IF],
        Some(ServerFeature::ConditionalKill)
    ),
    method!(
        "KILL_RESOURCES",
        Carrier::Command(COMMAND_TAG_KILL_RESOURCES),
        [C_KILL_RESOURCES]
    ),
    method!(
        "SUBSCRIBE_RESOURCE_EVENTS",
        Carrier::Command(COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS),
        [C_SUBSCRIBE_RESOURCE_EVENTS]
    ),
    method!(
        "SUBSCRIBE_EVENTS",
        Carrier::Frame(TYPE_SUBSCRIBE_EVENTS),
        [F_SUBSCRIBE_EVENTS_ONE, F_SUBSCRIBE_EVENTS_ALL]
    ),
    method!(
        "GET_STATE",
        Carrier::Command(COMMAND_TAG_GET_STATE),
        [C_GET_STATE]
    ),
    method!(
        "GET_METADATA",
        Carrier::Frame(TYPE_GET_METADATA),
        [F_GET_METADATA, F_WHOAMI]
    ),
    method!(
        "SET_METADATA",
        Carrier::Frame(TYPE_SET_METADATA),
        [
            F_METADATA_WRITE,
            F_SESSION_CREATE,
            F_KEEP_EMPTY_MARK,
            F_KEEP_EMPTY_CLEAR,
            F_KEEP_EMPTY_OTHER,
            F_CONFIG_RELOAD,
            F_RESULT_NAMESPACE_WRITE,
            F_SERVER_OWNED_WRITE,
        ]
    ),
    method!(
        "DELETE_METADATA",
        Carrier::Frame(TYPE_DELETE_METADATA),
        [
            F_METADATA_WRITE,
            F_RESULT_NAMESPACE_WRITE,
            F_SERVER_OWNED_WRITE
        ]
    ),
    method!(
        "LIST_METADATA",
        Carrier::Frame(TYPE_LIST_METADATA),
        [F_LIST_METADATA]
    ),
    method!(
        "SUBSCRIBE_METADATA",
        Carrier::Frame(TYPE_SUBSCRIBE_METADATA),
        [F_SUBSCRIBE_METADATA, F_RESULT_NAMESPACE_SUBSCRIBE]
    ),
];

/// Events addressed to one subscription rather than about a resource.
///
/// `journal_gap` tells a subscriber which journal `seq` range it missed; it
/// is never journaled itself (L1 §7.3).
pub static SERVER_EVENTS: &[EventSpec] = &[EventSpec {
    name: "journal_gap",
    tag: EVENT_TAG_JOURNAL_GAP,
}];

/// The events every kind shares: its own spawn and close, and `source_gap`
/// when the resource produced events faster than the server could journal
/// them (L1 §7.3).
pub static SUBSTRATE_EVENTS: &[EventSpec] = &[
    EventSpec {
        name: "pane_spawned",
        tag: EVENT_TAG_RESOURCE_SPAWNED,
    },
    EventSpec {
        name: "pane_closed",
        tag: EVENT_TAG_RESOURCE_CLOSED,
    },
    EventSpec {
        name: "source_gap",
        tag: EVENT_TAG_SOURCE_GAP,
    },
];

/// Every resource kind this build serves, in wire-tag order.
pub static KINDS: [KindSpec; 2] = [
    KindSpec {
        kind: ResourceKind::Terminal,
        name: "TERMINAL",
        gate: None,
        methods: &[
            method!("INPUT_KEY", Carrier::Frame(TYPE_INPUT_KEY), [F_INPUT]),
            method!("INPUT_PASTE", Carrier::Frame(TYPE_INPUT_PASTE), [F_INPUT]),
            method!("INPUT_MOUSE", Carrier::Frame(TYPE_INPUT_MOUSE), [F_INPUT]),
            method!("INPUT_FOCUS", Carrier::Frame(TYPE_INPUT_FOCUS), [F_INPUT]),
            MethodSpec {
                name: "INPUT_RAW",
                carrier: Carrier::Frame(INPUT_RAW_RESERVED_TYPE),
                rules: &[&F_INPUT],
                gate: None,
                shipped: false,
            },
            method!(
                "INPUT_TERMINAL_REPLY",
                Carrier::Frame(TYPE_INPUT_TERMINAL_REPLY),
                [F_INPUT],
                Some(ServerFeature::TerminalReply)
            ),
            method!(
                "RESIZE_TERMINAL",
                Carrier::Frame(TYPE_RESIZE_TERMINAL),
                [F_RESIZE_TERMINAL]
            ),
            method!(
                "HISTORY_REQUEST",
                Carrier::Frame(TYPE_HISTORY_REQUEST),
                [F_HISTORY_REQUEST]
            ),
            method!("FRAME_ACK", Carrier::Frame(TYPE_FRAME_ACK), [F_FRAME_ACK]),
            method!(
                "MOVE_RESOURCE",
                Carrier::Frame(TYPE_MOVE_RESOURCE),
                [F_MOVE_RESOURCE],
                Some(ServerFeature::MoveResource)
            ),
            method!(
                "GET_SCREEN",
                Carrier::Command(COMMAND_TAG_GET_SCREEN),
                [C_GET_SCREEN]
            ),
            method!(
                "ROUTE_INPUT",
                Carrier::Command(COMMAND_TAG_ROUTE_INPUT),
                [C_INPUT]
            ),
            method!(
                "APPLY_INPUT",
                Carrier::Command(COMMAND_TAG_APPLY_INPUT),
                [C_INPUT],
                Some(ServerFeature::AcknowledgedInput)
            ),
            method!(
                "PUT_FILE",
                Carrier::Command(COMMAND_TAG_PUT_FILE),
                [C_PUT_FILE],
                Some(ServerFeature::FileUpload)
            ),
            method!(
                "TRANSCRIBE",
                Carrier::Command(COMMAND_TAG_TRANSCRIBE),
                [C_TRANSCRIBE],
                Some(ServerFeature::Transcribe)
            ),
            method!(
                "GET_TERMINAL_STATE",
                Carrier::Command(COMMAND_TAG_GET_TERMINAL_STATE),
                [C_GET_TERMINAL_STATE]
            ),
            method!(
                "ACQUIRE_INPUT",
                Carrier::Command(COMMAND_TAG_ACQUIRE_INPUT),
                [C_INPUT_LEASE]
            ),
            method!(
                "RELEASE_INPUT",
                Carrier::Command(COMMAND_TAG_RELEASE_INPUT),
                [C_INPUT_LEASE]
            ),
            method!(
                "SIGNAL_TERMINAL",
                Carrier::Command(COMMAND_TAG_SIGNAL_TERMINAL),
                [C_SIGNAL_TERMINAL]
            ),
            method!(
                "REPORT_ASKED",
                Carrier::Command(COMMAND_TAG_REPORT_ASKED),
                [C_AGENT_REPORT]
            ),
            method!(
                "REPORT_AGENT_STATE",
                Carrier::Command(COMMAND_TAG_REPORT_AGENT_STATE),
                [C_AGENT_REPORT],
                Some(ServerFeature::ReportAgentState)
            ),
        ],
        events: &[
            EventSpec {
                name: "command_started",
                tag: EVENT_TAG_COMMAND_STARTED,
            },
            EventSpec {
                name: "command_finished",
                tag: EVENT_TAG_COMMAND_FINISHED,
            },
            EventSpec {
                name: "title_changed",
                tag: EVENT_TAG_TITLE_CHANGED,
            },
            EventSpec {
                name: "bell",
                tag: EVENT_TAG_BELL,
            },
            EventSpec {
                name: "dirty",
                tag: EVENT_TAG_DIRTY,
            },
            EventSpec {
                name: "idle",
                tag: EVENT_TAG_IDLE,
            },
            EventSpec {
                name: "terminal_control",
                tag: EVENT_TAG_TERMINAL_CONTROL,
            },
            EventSpec {
                name: "asked",
                tag: EVENT_TAG_ASKED,
            },
            EventSpec {
                name: "cwd_changed",
                tag: EVENT_TAG_CWD_CHANGED,
            },
        ],
        metadata_keys: &[
            RESOURCE_AGENT_KEY,
            RESOURCE_AGENT_SESSION_KEY,
            RESOURCE_PANE_OCCUPANT_KEY,
            RESOURCE_TAGS_KEY,
            RESOURCE_LINK_KEY,
        ],
    },
    KindSpec {
        kind: ResourceKind::AgentSession,
        name: "AGENT_SESSION",
        gate: Some(ServerFeature::ResourceKinds),
        methods: &[method!(
            "APPEND_RESOURCE_OUTPUT",
            Carrier::Command(COMMAND_TAG_APPEND_RESOURCE_OUTPUT),
            [C_APPEND_RESOURCE_OUTPUT],
            Some(ServerFeature::ResourceKinds)
        )],
        events: &[],
        metadata_keys: &[],
    },
];

/// Every catalog method: server-level, then substrate, then each kind's
/// facet in [`KINDS`] order.
pub fn methods() -> impl Iterator<Item = &'static MethodSpec> {
    SERVER_METHODS
        .iter()
        .chain(SUBSTRATE_METHODS)
        .chain(KINDS.iter().flat_map(|kind| kind.methods))
}

/// The catalog method carried by nested command `tag`, or `None` for a tag
/// this build does not allocate (which a decoder refuses and the classifier
/// would deny).
#[must_use]
pub fn command_method(tag: u8) -> Option<&'static MethodSpec> {
    methods().find(|method| method.carrier == Carrier::Command(tag))
}

/// The catalog method carried by client-to-server frame `type_byte`, or
/// `None` for a type that is unallocated, retired, or server-to-client.
#[must_use]
pub fn frame_method(type_byte: u8) -> Option<&'static MethodSpec> {
    methods().find(|method| method.carrier == Carrier::Frame(type_byte))
}

/// The catalog method named `name` (a wire name or a metadata key).
#[must_use]
pub fn method_named(name: &str) -> Option<&'static MethodSpec> {
    methods().find(|method| method.name == name)
}

/// The catalog entry for `kind`, or `None` for a kind this build does not
/// serve.
#[must_use]
pub fn kind_spec(kind: ResourceKind) -> Option<&'static KindSpec> {
    KINDS.iter().find(|spec| spec.kind == kind)
}

#[cfg(test)]
mod samples;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::GroupId;
    use crate::wire::frame::{ViewportInfo, encode_session_keep_empty};

    fn terminal() -> ResourceId {
        ResourceId::local(7)
    }

    fn row(frame: &FrameKind) -> &'static str {
        frame_rule(frame).case
    }

    fn command_row(command: Command) -> &'static str {
        row(&FrameKind::Command {
            request_id: 1,
            command,
        })
    }

    fn spawn(
        satellite: Option<&str>,
        owner_terminal: Option<ResourceId>,
        resource: Option<SpawnResource>,
    ) -> FrameKind {
        FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: satellite.map(SatelliteHost::new),
            owner_terminal,
            agent_session: None,
            initial_size: None,
            resource: resource.map(Box::new),
        }
    }

    fn agent(parent: ResourceId) -> SpawnResource {
        SpawnResource::agent_session(parent, "claude")
    }

    #[test]
    fn terminal_spawn_rows_follow_satellite_and_owner() {
        assert_eq!(row(&spawn(None, None, None)), F_SPAWN_LOCAL.case);
        assert_eq!(
            row(&spawn(None, Some(terminal()), None)),
            F_SPAWN_OWNED.case
        );
        assert_eq!(row(&spawn(Some("h"), None, None)), F_SPAWN_SATELLITE.case);
        assert_eq!(
            row(&spawn(Some("h"), Some(terminal()), None)),
            F_SPAWN_SATELLITE_OWNED.case
        );
        assert_eq!(
            classify_frame(&spawn(Some("h"), Some(terminal()), None)),
            Classification::Deny
        );
    }

    #[test]
    fn agent_session_spawn_rows_follow_parent_and_host() {
        assert_eq!(
            row(&spawn(None, None, Some(agent(terminal())))),
            F_SPAWN_AGENT_LOCAL.case
        );
        let remote_parent = ResourceId::satellite("h", 7);
        assert_eq!(
            row(&spawn(Some("h"), None, Some(agent(remote_parent.clone())))),
            F_SPAWN_AGENT_SATELLITE.case
        );
        // A local or different-host parent under a satellite spawn.
        assert_eq!(
            row(&spawn(Some("h"), None, Some(agent(terminal())))),
            F_UNCLASSIFIED.case
        );
        assert_eq!(
            row(&spawn(Some("other"), None, Some(agent(remote_parent)))),
            F_UNCLASSIFIED.case
        );
        // No row covers an agent session that also names an owner.
        assert_eq!(
            row(&spawn(None, Some(terminal()), Some(agent(terminal())))),
            F_UNCLASSIFIED.case
        );
    }

    #[test]
    fn a_kind_that_contradicts_its_binding_is_refused_first() {
        let mut parented_terminal = agent(terminal());
        parented_terminal.kind = ResourceKind::Terminal;
        let mut orphan_agent = agent(terminal());
        orphan_agent.parent = None;
        let mut orphan_unknown = agent(terminal());
        orphan_unknown.kind = ResourceKind::Unknown { tag: 9 };
        orphan_unknown.parent = None;
        for resource in [parented_terminal, orphan_agent, orphan_unknown] {
            assert_eq!(
                row(&spawn(None, None, Some(resource))),
                F_SPAWN_KIND_MISMATCH.case
            );
        }
    }

    fn set(scope: Scope, key: &str, value: &[u8]) -> FrameKind {
        FrameKind::SetMetadata {
            request_id: 1,
            scope,
            key: key.to_owned(),
            value: value.to_vec(),
        }
    }

    fn delete(key: &str) -> FrameKind {
        FrameKind::DeleteMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: key.to_owned(),
        }
    }

    fn subscribe(key: &str) -> FrameKind {
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: key.to_owned(),
        }
    }

    #[test]
    fn server_interpreted_global_writes_have_their_own_rows() {
        assert_eq!(
            row(&set(Scope::Global, SESSION_CREATE_KEY, b"{}")),
            F_SESSION_CREATE.case
        );
        assert_eq!(
            row(&set(Scope::Global, CONFIG_RELOAD_KEY, b"1")),
            F_CONFIG_RELOAD.case
        );
        let mark = encode_session_keep_empty("work", true);
        let unmark = encode_session_keep_empty("work", false);
        assert_eq!(
            row(&set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &mark)),
            F_KEEP_EMPTY_MARK.case
        );
        assert_eq!(
            row(&set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &unmark)),
            F_KEEP_EMPTY_CLEAR.case
        );
        for other in [b"work".as_slice(), b"work\0yes".as_slice()] {
            assert_eq!(
                row(&set(Scope::Global, SESSION_KEEP_EMPTY_KEY, other)),
                F_KEEP_EMPTY_OTHER.case
            );
        }
        // The special rows are Global-scoped; elsewhere the key is ordinary.
        assert_eq!(
            row(&set(Scope::Resource(terminal()), SESSION_CREATE_KEY, b"{}")),
            F_METADATA_WRITE.case
        );
    }

    #[test]
    fn server_owned_keys_are_not_writable() {
        let result = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}token");
        for key in [SESSION_CREATE_RESULT_KEY, result.as_str()] {
            assert_eq!(
                row(&set(Scope::Global, key, b"x")),
                F_RESULT_NAMESPACE_WRITE.case
            );
            assert_eq!(row(&delete(key)), F_RESULT_NAMESPACE_WRITE.case);
            assert_eq!(row(&subscribe(key)), F_RESULT_NAMESPACE_SUBSCRIBE.case);
        }
        for key in [RESOURCE_PANE_OCCUPANT_KEY, WHOAMI_KEY] {
            assert_eq!(
                row(&set(Scope::Resource(terminal()), key, b"x")),
                F_SERVER_OWNED_WRITE.case
            );
            assert_eq!(row(&delete(key)), F_SERVER_OWNED_WRITE.case);
        }
        for key in [CONFIG_RELOAD_KEY, SESSION_KEEP_EMPTY_KEY] {
            assert_eq!(row(&delete(key)), F_SERVER_OWNED_WRITE.case);
        }
        assert_eq!(row(&delete(SESSION_CREATE_KEY)), F_METADATA_WRITE.case);
        assert_eq!(
            row(&set(Scope::Resource(terminal()), RESOURCE_TAGS_KEY, b"[]")),
            F_METADATA_WRITE.case
        );
        assert_eq!(
            row(&subscribe(RESOURCE_AGENT_KEY)),
            F_SUBSCRIBE_METADATA.case
        );
    }

    #[test]
    fn attach_and_event_subscriptions_split_on_their_payload() {
        let attach = |target| FrameKind::Attach {
            attach_id: 1,
            target,
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        assert_eq!(row(&attach(AttachTarget::Last)), F_ATTACH.case);
        assert_eq!(
            row(&attach(AttachTarget::CreateIfMissing {
                name: "work".to_owned(),
                command: None,
                cwd: None,
            })),
            F_ATTACH_CREATE.case
        );
        assert_eq!(
            row(&FrameKind::SubscribeEvents {
                terminal: Some(terminal()),
                after_seq: None,
            }),
            F_SUBSCRIBE_EVENTS_ONE.case
        );
        assert_eq!(
            row(&FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            }),
            F_SUBSCRIBE_EVENTS_ALL.case
        );
    }

    #[test]
    fn commands_split_on_their_payload_and_the_envelope_defers() {
        assert_eq!(
            command_row(Command::DetachClients {
                session: Some("work".to_owned())
            }),
            C_DETACH_CLIENTS_SESSION.case
        );
        assert_eq!(
            command_row(Command::DetachClients { session: None }),
            C_DETACH_CLIENTS_ALL.case
        );
        assert_eq!(
            command_row(Command::GetPerf { reset: false }),
            C_GET_PERF.case
        );
        assert_eq!(
            command_row(Command::GetPerf { reset: true }),
            C_GET_PERF_RESET.case
        );
        assert_eq!(
            classify_command(&Command::Shutdown),
            Classification::Allow {
                verbs: Verbs::of(&[Verb::Signal]),
                subject: Subject::Global {
                    owner_uds_only: true
                },
            }
        );
        assert_eq!(F_COMMAND.classification(), Classification::Deny);
    }

    #[test]
    fn server_to_client_frames_are_wrong_direction() {
        assert_eq!(row(&FrameKind::Pong { nonce: 1 }), F_UNCLASSIFIED.case);
        assert_eq!(
            classify_frame(&FrameKind::AttachReady { attach_id: 1 }),
            Classification::Deny
        );
    }

    #[test]
    fn verb_set_helpers() {
        let all = Verbs::of(&Verb::ALL);
        assert_eq!(all.bits(), Verbs::KNOWN_BITS);
        assert!(!Verbs::of(&[Verb::Inventory, Verb::Observe]).mutates());
        assert!(Verbs::of(&[Verb::Observe, Verb::Bind]).mutates());
        assert!(Verbs::EMPTY.is_empty());
        assert_eq!(
            C_GET_PERF_RESET.requirement_label(),
            "OBSERVE+BIND",
            "labels list verbs in bit order"
        );
        assert_eq!(
            C_OPEN_LISTENER.requirement_label(),
            "SIGNAL + owner-UDS transport"
        );
    }
}
