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
    /// A Global L3 metadata key with its own entry: one the server
    /// intercepts or answers, or a consumer doorbell (config reload).
    Metadata(&'static str),
}

/// The `HELLO_OK.server_caps` word a feature bit lives in. Word 0 is closed
/// (ADR-0137), so a new feature lands in a trailing word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureWord {
    /// `server_caps.features`, the original word.
    Features,
    /// `server_caps.features_ext`, the first trailing word.
    FeaturesExt,
}

impl FeatureWord {
    /// The caps record field name, as `phux --capabilities --json` prints it.
    #[must_use]
    pub const fn field_name(self) -> &'static str {
        match self {
            Self::Features => "features",
            Self::FeaturesExt => "features_ext",
        }
    }
}

/// The feature bit a client must see advertised before relying on a method
/// or kind: which caps word it lives in, its mask there, and its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureGate {
    /// A bit in `server_caps.features`.
    Features(ServerFeature),
    /// A bit in `server_caps.features_ext`.
    FeaturesExt {
        /// The bit's value in the word (a mask, not an index).
        mask: u32,
        /// The spec constant, e.g. `"PATH_QUERY"`.
        wire_name: &'static str,
    },
}

impl FeatureGate {
    /// The caps word the bit lives in.
    #[must_use]
    pub const fn word(self) -> FeatureWord {
        match self {
            Self::Features(_) => FeatureWord::Features,
            Self::FeaturesExt { .. } => FeatureWord::FeaturesExt,
        }
    }

    /// The bit's value in its word (a mask, not an index).
    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Features(feature) => feature as u32,
            Self::FeaturesExt { mask, .. } => mask,
        }
    }

    /// The spec constant, e.g. `"LIST_DIRECTORY"`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Features(feature) => feature.wire_name(),
            Self::FeaturesExt { wire_name, .. } => wire_name,
        }
    }

    /// [`Self::wire_name`] lower-cased, as `phux status --json` lists it.
    #[must_use]
    pub fn snake_name(self) -> String {
        self.wire_name().to_ascii_lowercase()
    }
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
    /// method, if any, in whichever caps word it lives.
    pub gate: Option<FeatureGate>,
    /// Whether the codec and reference server implement it. `false` marks a
    /// spec-only allocation.
    pub shipped: bool,
    /// Whether an instance can end a process, eject a client, stop or
    /// re-exec the server, open a door into it, or release a held action
    /// (ADR-0128). MCP `destructiveHint`/`confirm` and CLI `--yes` derive
    /// from this; see [`command_is_dangerous`] and [`frame_is_dangerous`].
    pub dangerous: bool,
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
    /// Conservative: a method no row admits by verb counts as mutating, so a
    /// read-only hint never covers a denied write.
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
    pub gate: Option<FeatureGate>,
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
            dangerous: false,
        }
    };
    ($name:expr, $carrier:expr, [$($rule:ident),+ $(,)?]) => {
        method!($name, $carrier, [$($rule),+], None)
    };
}

/// `method` marked [`MethodSpec::dangerous`].
const fn dangerous(method: MethodSpec) -> MethodSpec {
    MethodSpec {
        dangerous: true,
        ..method
    }
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
        Some(FeatureGate::Features(ServerFeature::ListDirectory))
    ),
    method!(
        "PATH_QUERY",
        Carrier::Frame(TYPE_PATH_QUERY),
        [F_PATH_QUERY]
    ),
    dangerous(method!(
        "UPGRADE",
        Carrier::Command(COMMAND_TAG_UPGRADE),
        [C_UPGRADE]
    )),
    dangerous(method!(
        "SHUTDOWN",
        Carrier::Command(COMMAND_TAG_SHUTDOWN),
        [C_SHUTDOWN],
        Some(FeatureGate::Features(ServerFeature::Shutdown))
    )),
    dangerous(method!(
        "OPEN_LISTENER",
        Carrier::Command(COMMAND_TAG_OPEN_LISTENER),
        [C_OPEN_LISTENER],
        Some(FeatureGate::Features(ServerFeature::OpenListener))
    )),
    dangerous(method!(
        "DETACH_CLIENTS",
        Carrier::Command(COMMAND_TAG_DETACH_CLIENTS),
        [C_DETACH_CLIENTS_SESSION, C_DETACH_CLIENTS_ALL]
    )),
    method!(
        "GET_PERF",
        Carrier::Command(COMMAND_TAG_GET_PERF),
        [C_GET_PERF, C_GET_PERF_RESET],
        Some(FeatureGate::Features(ServerFeature::GetPerf))
    ),
    method!(
        "PERF_CONTROL",
        Carrier::Command(COMMAND_TAG_PERF_CONTROL),
        [C_PERF_CONTROL],
        Some(FeatureGate::FeaturesExt {
            mask: ServerFeatureExt::PerfControl.mask(),
            wire_name: ServerFeatureExt::PerfControl.wire_name(),
        })
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
        Some(FeatureGate::Features(ServerFeature::KeepEmptySessions))
    ),
    // A TUI doorbell: the server stores the nonce like any value, and §6
    // classifies the write as SIGNAL because it makes consumers reload.
    dangerous(method!(
        CONFIG_RELOAD_KEY,
        Carrier::Metadata(CONFIG_RELOAD_KEY),
        [F_CONFIG_RELOAD]
    )),
    // A decision is server-intercepted and stored nowhere. Approving one
    // releases a held SIGNAL action, so it is dangerous; denying releases
    // nothing (`frame_is_dangerous`).
    dangerous(method!(
        APPROVAL_DECIDE_KEY_PREFIX,
        Carrier::Metadata(APPROVAL_DECIDE_KEY_PREFIX),
        [F_APPROVAL_DECIDE, F_APPROVAL_DECIDE_OTHER],
        Some(FeatureGate::Features(ServerFeature::Approvals))
    )),
    method!(
        WHOAMI_KEY,
        Carrier::Metadata(WHOAMI_KEY),
        [F_WHOAMI],
        Some(FeatureGate::Features(ServerFeature::Whoami))
    ),
    // The organization commands (L1 §5.7): records named by keys, edited
    // under expected revisions, answered with receipts or refusals.
    method!(
        "GET_ORGANIZATION",
        Carrier::Command(COMMAND_TAG_GET_ORGANIZATION),
        [C_GET_ORGANIZATION],
        ORGANIZATION
    ),
    method!(
        "SESSION_CREATE",
        Carrier::Command(COMMAND_TAG_SESSION_CREATE),
        [C_SESSION_CREATE],
        ORGANIZATION
    ),
    method!(
        "SESSION_RENAME",
        Carrier::Command(COMMAND_TAG_SESSION_RENAME),
        [C_SESSION_ORGANIZE],
        ORGANIZATION
    ),
    method!(
        "SESSION_SET_LIFETIME",
        Carrier::Command(COMMAND_TAG_SESSION_SET_LIFETIME),
        [C_SESSION_ORGANIZE],
        ORGANIZATION
    ),
    dangerous(method!(
        "SESSION_DELETE",
        Carrier::Command(COMMAND_TAG_SESSION_DELETE),
        [C_SESSION_DELETE],
        ORGANIZATION
    )),
    method!(
        "RESOURCE_CREATE",
        Carrier::Command(COMMAND_TAG_RESOURCE_CREATE),
        [C_RESOURCE_CREATE],
        ORGANIZATION
    ),
    method!(
        "RESOURCE_MOVE",
        Carrier::Command(COMMAND_TAG_RESOURCE_MOVE),
        [C_RESOURCE_MOVE],
        ORGANIZATION
    ),
    method!(
        "RESOURCE_MOVE_MANY",
        Carrier::Command(COMMAND_TAG_RESOURCE_MOVE_MANY),
        [C_RESOURCE_MOVE],
        ORGANIZATION
    ),
    method!(
        "RESOURCE_RENAME",
        Carrier::Command(COMMAND_TAG_RESOURCE_RENAME),
        [C_RESOURCE_RENAME],
        ORGANIZATION
    ),
    dangerous(method!(
        "RESOURCE_TERMINATE",
        Carrier::Command(COMMAND_TAG_RESOURCE_TERMINATE),
        [C_RESOURCE_TERMINATE],
        ORGANIZATION
    )),
    dangerous(method!(
        "RESOURCE_REMOVE",
        Carrier::Command(COMMAND_TAG_RESOURCE_REMOVE),
        [C_RESOURCE_REMOVE],
        ORGANIZATION
    )),
    method!(
        "RESOURCE_RELAUNCH",
        Carrier::Command(COMMAND_TAG_RESOURCE_RELAUNCH),
        [C_RESOURCE_RELAUNCH],
        ORGANIZATION
    ),
    method!(
        "INBOX_RESOLVE",
        Carrier::Command(COMMAND_TAG_INBOX_RESOLVE),
        [C_SESSION_ORGANIZE],
        ORGANIZATION
    ),
    method!(
        "ORGANIZATION_UNDO",
        Carrier::Command(COMMAND_TAG_ORGANIZATION_UNDO),
        [C_ORGANIZATION_UNDO],
        ORGANIZATION
    ),
    method!(
        "ARRANGEMENT_GET",
        Carrier::Command(COMMAND_TAG_ARRANGEMENT_GET),
        [C_ARRANGEMENT_GET],
        ORGANIZATION
    ),
    method!(
        "ARRANGEMENT_REPLACE",
        Carrier::Command(COMMAND_TAG_ARRANGEMENT_REPLACE),
        [C_ARRANGEMENT_REPLACE],
        ORGANIZATION
    ),
    // Delegated agent credentials (ADR-0163 §5): the owner hands an agent a
    // grant narrower than its own. New, so no stored grant reached them
    // before; served whatever `ORGANIZATION` says.
    method!(
        "DELEGATE_AGENT",
        Carrier::Command(COMMAND_TAG_DELEGATE_AGENT),
        [C_DELEGATION]
    ),
    method!(
        "REVOKE_DELEGATION",
        Carrier::Command(COMMAND_TAG_REVOKE_DELEGATION),
        [C_DELEGATION]
    ),
    dangerous(method!(
        "SESSION_KILL",
        Carrier::Command(COMMAND_TAG_SESSION_KILL),
        [C_SESSION_DELETE],
        Some(FeatureGate::FeaturesExt {
            mask: ServerFeatureExt::SessionKill.mask(),
            wire_name: ServerFeatureExt::SessionKill.wire_name(),
        })
    )),
    method!(
        "SESSION_SET_PROJECT",
        Carrier::Command(COMMAND_TAG_SESSION_SET_PROJECT),
        [C_SESSION_ORGANIZE],
        Some(FeatureGate::FeaturesExt {
            mask: ServerFeatureExt::SessionProject.mask(),
            wire_name: ServerFeatureExt::SessionProject.wire_name(),
        })
    ),
];
