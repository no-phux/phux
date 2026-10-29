// -----------------------------------------------------------------------------
// Verbs, subjects, and classifications.
// -----------------------------------------------------------------------------

/// One closed verb of `docs/remote.md#who-can-do-what`. The discriminant is the
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
    /// Rename, move membership, edit arrangements, resolve intake, or undo an
    /// organization edit (scope schema v2).
    Organize = 0x40,
    /// Terminate an activation, remove a resource record, or delete a
    /// session (scope schema v2).
    Terminate = 0x80,
}

impl Verb {
    /// Every verb, in bit order.
    pub const ALL: [Self; 8] = [
        Self::Inventory,
        Self::Observe,
        Self::Create,
        Self::Bind,
        Self::Input,
        Self::Signal,
        Self::Organize,
        Self::Terminate,
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
            Self::Organize => "ORGANIZE",
            Self::Terminate => "TERMINATE",
        }
    }
}

/// A set of [`Verb`]s: the bitset a scope grant carries (workload-auth §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Verbs(u8);

impl Verbs {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// The bits a grant may carry under the current scope schema (v2): all
    /// eight verbs.
    pub const KNOWN_BITS: u8 = 0xFF;

    /// The bits a scope-schema v1 grant may carry. `0xC0` is unknown in v1,
    /// and a v1 grant carrying it is rejected, never ignored.
    pub const V1_KNOWN_BITS: u8 = 0x3F;

    /// A v1 grant's verbs as schema v2 reads them (workload-auth §5):
    /// `SIGNAL` implies `TERMINATE` and `CREATE` implies `ORGANIZE`, so a
    /// registry written before the split reaches the organization
    /// equivalents of what it could already do. Legacy commands keep their
    /// exact requirements, so the implication grants no legacy operation.
    #[must_use]
    pub const fn with_v1_implications(self) -> Self {
        let mut bits = self.0;
        if bits & Verb::Signal.bit() != 0 {
            bits |= Verb::Terminate.bit();
        }
        if bits & Verb::Create.bit() != 0 {
            bits |= Verb::Organize.bit();
        }
        Self(bits)
    }

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
    /// The held action a `phux.approval.decide/v1/<id>` key names
    /// (ADR-0128): every subject the held command needs, resolved
    /// side-effect-free under the current topology from the pending
    /// approval. An id with no pending approval resolves to nothing, so it
    /// is refused like an absent target.
    HeldAction,
    /// The organization records the grant inventories, as a filtered page:
    /// a session through its Group, a resource through its session, and
    /// authority-wide totals and the Intake designation only with the Global
    /// selector.
    OrganizationView,
    /// The session the command's `SessionKey` names, resolved
    /// side-effect-free to its live Group. An absent key resolves to no
    /// subject, so it is refused like an unauthorized one.
    KeyedSession,
    /// The resource the command's `ResourceKey` names, matched through its
    /// current session's Group.
    KeyedResource,
    /// Every session a membership move takes a resource from, and the
    /// destination session; all-or-nothing.
    MoveSessions,
    /// Every session and resource the undone operation's recorded inverse
    /// touches, under current membership.
    UndoneOperation,
    /// The session that owns the arrangement.
    ArrangementOwner,
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
