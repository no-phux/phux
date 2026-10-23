//! Extended: extracted from the shared module.


/// Features in the second, trailing server-capability word (ADR-0137).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ServerFeatureExt {
    /// Host-side file/directory browsing and recursive fuzzy path search.
    PathQuery = 0x0000_0001,
    /// `RESIZE_TERMINAL` carries and applies an optional cell pixel size
    /// (fields 4/5, ADR-0145), so a client can size panes without a
    /// viewport vote.
    ResizeCellPx = 0x0000_0002,
    /// A kill's reply follows a commit that is the whole visible teardown
    /// (`docs/spec/L1.md` §5.2): the next `GET_STATE` already omits the
    /// killed resources, so a client need not wait for their reap.
    CommittedTeardown = 0x0000_0004,
    /// `ATTACH_RESOURCE` accepts an agent cursor and explicitly distinguishes
    /// a resumed suffix from a full retained-ring replacement.
    AgentStreamResume = 0x0000_0008,
    /// The server accepts the organization commands (`docs/spec/L1.md`
    /// §5.7) and enforces every rule they carry: keyed sessions and
    /// resources, revisions, receipts, refusals, and the organization
    /// events. A client that does not see it treats organization as
    /// read-only.
    Organization = 0x0000_0010,
    /// The server accepts `SESSION_KILL` (`docs/spec/L1.md` §5.7): one
    /// command ends every member of a session and deletes it. Advertised
    /// only beside [`Self::Organization`].
    SessionKill = 0x0000_0020,
    /// Dedicated SDK-owned `AgentSession` creation and typed native commands.
    NativeAgent = 0x0000_0040,
    /// The server accepts `PERF_CONTROL`: start and stop a span trace and
    /// switch heap accounting. A client MUST see the bit before sending the
    /// command.
    PerfControl = 0x0000_0080,
    /// The server accepts `SESSION_SET_PROJECT` (`docs/spec/L1.md` §5.7):
    /// link a session into a named project after creation, or clear the
    /// link. Advertised only beside [`Self::Organization`]; a client MUST see
    /// the bit before sending the command.
    SessionProject = 0x0000_0100,
    /// The server answers `GET_FILE`: bounded chunks of any host file the
    /// serving user can read, addressed to this server or a named satellite.
    FileDownload = 0x0000_0200,
    /// The server answers `CHECKOUT`: typed Git reads and index writes on
    /// the checkout a named Terminal is in, resolved from the Terminal's
    /// live working directory.
    Checkout = 0x0000_0400,
    /// Executing-host native integration inventory and keyed launch.
    NativeAgentLaunch = 0x0000_0800,
    /// The server answers `SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`, and
    /// `SERVICE_HTTP_CLOSE`: HTTP exchanges with a named service on its host
    /// (`opencode`) or on a named satellite's, authorized by the connection.
    HostServiceHttp = 0x0000_1000,
    /// Resource creation commits placement with membership and launch intent,
    /// and returns stable entry identity and current execution in its receipt.
    CreatePlacement = 0x0000_2000,
    /// Connected-server public workflow description and nested schemas.
    ApiDescription = 0x0000_4000,
    /// Exact durable resource filter on `GET_ORGANIZATION`, independent of engines.
    DurableResourceInspection = 0x0000_8000,
}


impl ServerFeatureExt {
    /// Every known extended feature, in bit order.
    pub const ALL: &'static [Self] = &[
        Self::PathQuery,
        Self::ResizeCellPx,
        Self::CommittedTeardown,
        Self::AgentStreamResume,
        Self::Organization,
        Self::SessionKill,
        Self::NativeAgent,
        Self::PerfControl,
        Self::SessionProject,
        Self::FileDownload,
        Self::Checkout,
        Self::NativeAgentLaunch,
        Self::HostServiceHttp,
        Self::CreatePlacement,
        Self::ApiDescription,
        Self::DurableResourceInspection,
    ];

    /// The `docs/spec/proto.md` §6.2 constant, e.g. `"PATH_QUERY"`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::PathQuery => "PATH_QUERY",
            Self::ResizeCellPx => "RESIZE_CELL_PX",
            Self::CommittedTeardown => "COMMITTED_TEARDOWN",
            Self::AgentStreamResume => "AGENT_STREAM_RESUME",
            Self::Organization => "ORGANIZATION",
            Self::SessionKill => "SESSION_KILL",
            Self::NativeAgent => "NATIVE_AGENT",
            Self::PerfControl => "PERF_CONTROL",
            Self::SessionProject => "SESSION_PROJECT",
            Self::FileDownload => "FILE_DOWNLOAD",
            Self::Checkout => "CHECKOUT",
            Self::NativeAgentLaunch => "NATIVE_AGENT_LAUNCH",
            Self::HostServiceHttp => "HOST_SERVICE_HTTP",
            Self::CreatePlacement => "CREATE_PLACEMENT",
            Self::ApiDescription => "API_DESCRIPTION",
            Self::DurableResourceInspection => "DURABLE_RESOURCE_INSPECTION",
        }
    }

    /// The bit's value in `features_ext` (a mask, not an index).
    #[must_use]
    pub const fn mask(self) -> u32 {
        self as u32
    }
}


/// Known bits in `HELLO_OK.server_caps.features_ext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ServerFeatureExtSet(u32);


impl ServerFeatureExtSet {
    const KNOWN: u32 = ServerFeatureExt::PathQuery as u32
        | ServerFeatureExt::ResizeCellPx as u32
        | ServerFeatureExt::CommittedTeardown as u32
        | ServerFeatureExt::AgentStreamResume as u32
        | ServerFeatureExt::Organization as u32
        | ServerFeatureExt::SessionKill as u32
        | ServerFeatureExt::NativeAgent as u32
        | ServerFeatureExt::PerfControl as u32
        | ServerFeatureExt::SessionProject as u32
        | ServerFeatureExt::FileDownload as u32
        | ServerFeatureExt::Checkout as u32
        | ServerFeatureExt::NativeAgentLaunch as u32
        | ServerFeatureExt::HostServiceHttp as u32
        | ServerFeatureExt::CreatePlacement as u32
        | ServerFeatureExt::ApiDescription as u32
        | ServerFeatureExt::DurableResourceInspection as u32;

    /// Known extended features in this set, in bit order.
    pub fn iter(self) -> impl Iterator<Item = ServerFeatureExt> {
        ServerFeatureExt::ALL
            .iter()
            .copied()
            .filter(move |feature| self.contains(*feature))
    }

    /// No extended features.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Every known extended feature.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Construct a set from known features.
    #[must_use]
    pub const fn with(features: &[ServerFeatureExt]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < features.len() {
            bits |= features[i] as u32;
            i += 1;
        }
        Self(bits)
    }

    /// Copy with one feature cleared.
    #[must_use]
    pub const fn without(self, feature: ServerFeatureExt) -> Self {
        Self(self.0 & !(feature as u32))
    }

    /// Whether this set contains a feature.
    #[must_use]
    pub const fn contains(self, feature: ServerFeatureExt) -> bool {
        self.0 & (feature as u32) != 0
    }

    /// Whether no extended feature is present.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Known bits to write to the second word.
    #[must_use]
    pub const fn as_wire(self) -> u32 {
        self.0 & Self::KNOWN
    }

    /// Read known bits, ignoring future additions.
    #[must_use]
    pub const fn from_wire(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }
}
