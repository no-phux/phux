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
    /// `server_caps.features_ext2`, the second trailing word.
    FeaturesExt2,
}

impl FeatureWord {
    /// The caps record field name, as `phux --capabilities --json` prints it.
    #[must_use]
    pub const fn field_name(self) -> &'static str {
        match self {
            Self::Features => "features",
            Self::FeaturesExt => "features_ext",
            Self::FeaturesExt2 => "features_ext2",
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
    /// A bit in the current executing peer's third capability word.
    FeaturesExt2(crate::caps::ServerFeatureExt2),
}

impl FeatureGate {
    /// The gate on a `server_caps.features_ext` bit.
    #[must_use]
    pub const fn ext(feature: ServerFeatureExt) -> Self {
        Self::FeaturesExt {
            mask: feature.mask(),
            wire_name: feature.wire_name(),
        }
    }

    /// Gate a method on the third capability word.
    #[must_use]
    pub const fn ext2(feature: crate::caps::ServerFeatureExt2) -> Self {
        Self::FeaturesExt2(feature)
    }

    /// Whether this bit is advertised in its own negotiated caps word.
    /// Zero masks are never advertised; unknown bits have already been
    /// removed by the typed feature sets. This is not authorization.
    #[must_use]
    pub const fn advertised(
        self,
        features: crate::caps::ServerFeatureSet,
        features_ext: crate::caps::ServerFeatureExtSet,
        features_ext2: crate::caps::ServerFeatureExt2Set,
    ) -> bool {
        match self {
            Self::Features(feature) => features.contains(feature),
            Self::FeaturesExt2(feature) => features_ext2.contains(feature),
            Self::FeaturesExt { mask, .. } => {
                mask != 0 && features_ext.as_wire() & mask == mask
            }
        }
    }

    /// The caps word the bit lives in.
    #[must_use]
    pub const fn word(self) -> FeatureWord {
        match self {
            Self::Features(_) => FeatureWord::Features,
            Self::FeaturesExt { .. } => FeatureWord::FeaturesExt,
            Self::FeaturesExt2(_) => FeatureWord::FeaturesExt2,
        }
    }

    /// The bit's value in its word (a mask, not an index).
    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Features(feature) => feature as u32,
            Self::FeaturesExt { mask, .. } => mask,
            Self::FeaturesExt2(feature) => feature.mask(),
        }
    }

    /// The spec constant, e.g. `"LIST_DIRECTORY"`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Features(feature) => feature.wire_name(),
            Self::FeaturesExt { wire_name, .. } => wire_name,
            Self::FeaturesExt2(feature) => feature.wire_name(),
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
    /// The kind whose app serves this host-addressed method, though it names
    /// no resource; see [`command_served_by`]. `None` for the host's own
    /// methods and for facet methods, whose kind is their row's.
    pub served_by: Option<ResourceKind>,
}

impl MethodSpec {
    /// Whether this method's ordinary feature gate is advertised. A method
    /// without a gate needs no bit. Does not judge payload, route, kind,
    /// transport, scope, or authority, and does not imply it is shipped.
    #[must_use]
    pub fn advertised(
        &self,
        features: crate::caps::ServerFeatureSet,
        features_ext: crate::caps::ServerFeatureExtSet,
        features_ext2: crate::caps::ServerFeatureExt2Set,
    ) -> bool {
        self.gate.is_none_or(|gate| gate.advertised(features, features_ext, features_ext2))
    }

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

/// `INPUT_RAW`'s frame type: allocated but spec-only, so the codec
/// declares no constant for it.
const INPUT_RAW_RESERVED_TYPE: u8 = 0x13;

/// A method table, one entry per method: its name, its carrier, every row
/// an instance can land on, then modifiers. `feature(BIT)` and `ext(BIT)`
/// gate it on a `features` or `features_ext` bit, `served_by(KIND)` sets
/// [`MethodSpec::served_by`], `dangerous` [`MethodSpec::dangerous`], and
/// `unshipped` marks a spec-only allocation.
macro_rules! methods {
    ($(
        $name:expr => $carrier:ident($tag:expr) [$($rule:ident),+ $(,)?]
        $($modifier:ident $(($($arg:tt)*))?)*;
    )*) => {
        &[$(methods!(@apply MethodSpec {
            name: $name,
            carrier: Carrier::$carrier($tag),
            rules: &[$(&$rule),+],
            gate: None,
            shipped: true,
            dangerous: false,
            served_by: None,
        }; $($modifier $(($($arg)*))?)*)),*]
    };
    (@apply $spec:expr;) => {
        $spec
    };
    (@apply $spec:expr; feature($bit:ident) $($rest:tt)*) => {
        methods!(@apply MethodSpec {
            gate: Some(FeatureGate::Features(ServerFeature::$bit)),
            ..$spec
        }; $($rest)*)
    };
    (@apply $spec:expr; ext($bit:ident) $($rest:tt)*) => {
        methods!(@apply MethodSpec {
            gate: Some(FeatureGate::ext(ServerFeatureExt::$bit)),
            ..$spec
        }; $($rest)*)
    };
    (@apply $spec:expr; ext2($bit:ident) $($rest:tt)*) => {
        methods!(@apply MethodSpec {
            gate: Some(FeatureGate::ext2(crate::caps::ServerFeatureExt2::$bit)),
            ..$spec
        }; $($rest)*)
    };
    (@apply $spec:expr; served_by($kind:ident) $($rest:tt)*) => {
        methods!(@apply MethodSpec { served_by: Some(ResourceKind::$kind), ..$spec }; $($rest)*)
    };
    (@apply $spec:expr; dangerous $($rest:tt)*) => {
        methods!(@apply MethodSpec { dangerous: true, ..$spec }; $($rest)*)
    };
    (@apply $spec:expr; unshipped $($rest:tt)*) => {
        methods!(@apply MethodSpec { shipped: false, ..$spec }; $($rest)*)
    };
}

/// An event table, one `name => tag;` entry per event.
macro_rules! events {
    ($($name:literal => $tag:ident;)*) => {
        &[$(EventSpec { name: $name, tag: $tag }),*]
    };
}

/// Methods addressed to the server or the connection, not to one resource.
///
/// These are the handshake, session attach, server lifecycle, telemetry,
/// and the Global metadata keys with entries of their own
/// ([`Carrier::Metadata`]).
pub static SERVER_METHODS: &[MethodSpec] = methods! {
    "CREDENTIALS" => Command(COMMAND_TAG_CREDENTIALS) [C_CREDENTIALS] ext(Credentials) dangerous;
    "GET_CREATION" => Command(COMMAND_TAG_GET_CREATION) [C_GET_CREATION] ext(ApiDescription);
    "DESCRIBE_API" => Command(COMMAND_TAG_DESCRIBE_API) [C_API_DESCRIBE] ext(ApiDescription);
    // The macOS privacy grants the server holds; owner socket only.
    "PRIVACY_STATUS" => Command(COMMAND_TAG_PRIVACY_STATUS) [C_PRIVACY_STATUS] ext(PrivacyStatus);
    // What works on the host, with reasons and fixes.
    "HOST_CHECKLIST" => Command(COMMAND_TAG_HOST_CHECKLIST) [C_HOST_CHECKLIST] ext(HostChecklist);
    "HELLO" => Frame(TYPE_HELLO) [F_HELLO];
    "PING" => Frame(TYPE_PING) [F_PING];
    "ATTACH" => Frame(TYPE_ATTACH) [F_ATTACH, F_ATTACH_CREATE];
    "DETACH" => Frame(TYPE_DETACH) [F_DETACH];
    "VIEWPORT_RESIZE" => Frame(TYPE_VIEWPORT_RESIZE) [F_VIEWPORT_RESIZE];
    "VIEWPORT_COLORS" => Frame(TYPE_VIEWPORT_COLORS) [F_VIEWPORT_COLORS] ext(ViewportColors);
    "COMMAND" => Frame(TYPE_COMMAND) [F_COMMAND];
    "LIST_DIRECTORY" => Frame(TYPE_LIST_DIRECTORY) [F_LIST_DIRECTORY] feature(ListDirectory);
    "PATH_QUERY" => Frame(TYPE_PATH_QUERY) [F_PATH_QUERY] ext(PathQuery);
    // A host's screen mirrors: list, start, stop; Rune's macOS runner
    // reports its targets and learns its mirrors through MIRROR_PROVIDE.
    // Addressed to the host, served by the app of the surfaces they make.
    "MIRROR" => Command(COMMAND_TAG_MIRROR) [C_MIRROR] ext(Mirrors) served_by(PixelSurface);
    "MIRROR_PROVIDE" => Command(COMMAND_TAG_MIRROR_PROVIDE) [C_MIRROR_PROVIDE]
        ext(Mirrors) served_by(PixelSurface);
    "GET_FILE" => Command(COMMAND_TAG_GET_FILE) [C_GET_FILE] ext(FileDownload);
    "SERVICE_HTTP_OPEN" => Command(COMMAND_TAG_SERVICE_HTTP_OPEN) [C_SERVICE_HTTP]
        ext(HostServiceHttp);
    "SERVICE_HTTP_READ" => Command(COMMAND_TAG_SERVICE_HTTP_READ) [C_SERVICE_HTTP]
        ext(HostServiceHttp);
    "SERVICE_HTTP_CLOSE" => Command(COMMAND_TAG_SERVICE_HTTP_CLOSE) [C_SERVICE_HTTP]
        ext(HostServiceHttp);
    // TCP port forwarding (`docs/ssh.md#forwarding-a-port`).
    "FORWARD_OPEN" => Command(COMMAND_TAG_FORWARD_OPEN) [C_FORWARD] ext(ForwardTcp);
    "FORWARD_READ" => Command(COMMAND_TAG_FORWARD_READ) [C_FORWARD] ext(ForwardTcp);
    "FORWARD_WRITE" => Command(COMMAND_TAG_FORWARD_WRITE) [C_FORWARD] ext(ForwardTcp);
    "FORWARD_CLOSE" => Command(COMMAND_TAG_FORWARD_CLOSE) [C_FORWARD] ext(ForwardTcp);
    // ssh-agent forwarding (`docs/ssh.md#ssh-agent`).
    "AGENT_OFFER" => Command(COMMAND_TAG_AGENT_OFFER) [C_FORWARD] ext(ForwardAgent);
    "AGENT_REPLY" => Command(COMMAND_TAG_AGENT_REPLY) [C_FORWARD] ext(ForwardAgent);
    "UPGRADE" => Command(COMMAND_TAG_UPGRADE) [C_UPGRADE] dangerous;
    "SHUTDOWN" => Command(COMMAND_TAG_SHUTDOWN) [C_SHUTDOWN] feature(Shutdown) dangerous;
    "OPEN_LISTENER" => Command(COMMAND_TAG_OPEN_LISTENER) [C_OPEN_LISTENER]
        feature(OpenListener) dangerous;
    "DETACH_CLIENTS" => Command(COMMAND_TAG_DETACH_CLIENTS)
        [C_DETACH_CLIENTS_SESSION, C_DETACH_CLIENTS_ALL] dangerous;
    "GET_PERF" => Command(COMMAND_TAG_GET_PERF) [C_GET_PERF, C_GET_PERF_RESET] feature(GetPerf);
    "PERF_CONTROL" => Command(COMMAND_TAG_PERF_CONTROL) [C_PERF_CONTROL] ext(PerfControl);
    SESSION_CREATE_KEY => Metadata(SESSION_CREATE_KEY) [F_SESSION_CREATE];
    // Session rename is server-intercepted, but the scope table gives it no row of its
    // own: a write lands on the ordinary metadata-write row (BIND on Global).
    SESSION_NAME_KEY => Metadata(SESSION_NAME_KEY) [F_METADATA_WRITE];
    // The writer's view size lock, applied to its attachment and stored nowhere; the
    // ordinary metadata-write row (BIND on Global) covers it.
    VIEW_SIZE_LOCK_KEY => Metadata(VIEW_SIZE_LOCK_KEY) [F_METADATA_WRITE];
    SESSION_KEEP_EMPTY_KEY => Metadata(SESSION_KEEP_EMPTY_KEY)
        [F_KEEP_EMPTY_MARK, F_KEEP_EMPTY_CLEAR, F_KEEP_EMPTY_OTHER] feature(KeepEmptySessions);
    // A TUI doorbell: the server stores the nonce like any value, and the scope table
    // classifies the write as SIGNAL because it makes consumers reload.
    CONFIG_RELOAD_KEY => Metadata(CONFIG_RELOAD_KEY) [F_CONFIG_RELOAD] dangerous;
    // A decision is server-intercepted and stored nowhere. Approving one
    // releases a held SIGNAL action, so it is dangerous; denying releases
    // nothing (`frame_is_dangerous`).
    APPROVAL_DECIDE_KEY_PREFIX => Metadata(APPROVAL_DECIDE_KEY_PREFIX)
        [F_APPROVAL_DECIDE, F_APPROVAL_DECIDE_OTHER] feature(Approvals) dangerous;
    WHOAMI_KEY => Metadata(WHOAMI_KEY) [F_WHOAMI] feature(Whoami);
    // The organization commands: records named by keys, edited
    // under expected revisions, answered with receipts or refusals.
    "GET_ORGANIZATION" => Command(COMMAND_TAG_GET_ORGANIZATION) [C_GET_ORGANIZATION]
        ext(Organization);
    "SESSION_CREATE" => Command(COMMAND_TAG_SESSION_CREATE) [C_SESSION_CREATE] ext(Organization);
    "SESSION_RENAME" => Command(COMMAND_TAG_SESSION_RENAME) [C_SESSION_ORGANIZE] ext(Organization);
    "SESSION_SET_LIFETIME" => Command(COMMAND_TAG_SESSION_SET_LIFETIME) [C_SESSION_ORGANIZE]
        ext(Organization);
    "SESSION_DELETE" => Command(COMMAND_TAG_SESSION_DELETE) [C_SESSION_DELETE]
        ext(Organization) dangerous;
    "RESOURCE_CREATE" => Command(COMMAND_TAG_RESOURCE_CREATE)
        [C_RESOURCE_CREATE, C_RESOURCE_CREATE_PLACED] ext(Organization);
    "RESOURCE_MOVE" => Command(COMMAND_TAG_RESOURCE_MOVE) [C_RESOURCE_MOVE] ext(Organization);
    "RESOURCE_MOVE_MANY" => Command(COMMAND_TAG_RESOURCE_MOVE_MANY) [C_RESOURCE_MOVE]
        ext(Organization);
    "RESOURCE_RENAME" => Command(COMMAND_TAG_RESOURCE_RENAME) [C_RESOURCE_RENAME] ext(Organization);
    "RESOURCE_TERMINATE" => Command(COMMAND_TAG_RESOURCE_TERMINATE) [C_RESOURCE_TERMINATE]
        ext(Organization) dangerous;
    "RESOURCE_REMOVE" => Command(COMMAND_TAG_RESOURCE_REMOVE) [C_RESOURCE_REMOVE]
        ext(Organization) dangerous;
    "RESOURCE_RELAUNCH" => Command(COMMAND_TAG_RESOURCE_RELAUNCH) [C_RESOURCE_RELAUNCH]
        ext2(ArchivedLaunchAdmission);
    "INBOX_RESOLVE" => Command(COMMAND_TAG_INBOX_RESOLVE) [C_SESSION_ORGANIZE] ext(Organization);
    "ORGANIZATION_UNDO" => Command(COMMAND_TAG_ORGANIZATION_UNDO) [C_ORGANIZATION_UNDO]
        ext(Organization);
    "ARRANGEMENT_GET" => Command(COMMAND_TAG_ARRANGEMENT_GET) [C_ARRANGEMENT_GET] ext(Organization);
    "ARRANGEMENT_REPLACE" => Command(COMMAND_TAG_ARRANGEMENT_REPLACE) [C_ARRANGEMENT_REPLACE]
        ext(Organization);
    // Delegated agent credentials (ADR-0163 §5): the owner hands an agent a
    // grant narrower than its own. New, so no stored grant reached them
    // before; served whatever `ORGANIZATION` says.
    "DELEGATE_AGENT" => Command(COMMAND_TAG_DELEGATE_AGENT) [C_DELEGATION];
    "REVOKE_DELEGATION" => Command(COMMAND_TAG_REVOKE_DELEGATION) [C_DELEGATION];
    "SESSION_KILL" => Command(COMMAND_TAG_SESSION_KILL) [C_SESSION_DELETE]
        ext(SessionKill) dangerous;
    "SESSION_ENSURE" => Command(COMMAND_TAG_SESSION_ENSURE) [C_SESSION_CREATE]
        ext2(SessionEnsure);
    "SESSION_SET_PROJECT" => Command(COMMAND_TAG_SESSION_SET_PROJECT) [C_SESSION_ORGANIZE]
        ext(SessionProject);
    "ARRANGEMENT_LOCK" => Command(COMMAND_TAG_ARRANGEMENT_LOCK) [C_ARRANGEMENT_REPLACE]
        ext(TabLock);
};
