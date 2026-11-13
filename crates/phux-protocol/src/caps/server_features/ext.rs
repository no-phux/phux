//! [`ServerFeatureExt`]: the bits of the trailing capability word.

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
    /// (`docs/protocol.md#commands-events-resources`): the next `GET_STATE` already omits the
    /// killed resources, so a client need not wait for their reap.
    CommittedTeardown = 0x0000_0004,
    /// `ATTACH_RESOURCE` accepts an agent cursor and explicitly distinguishes
    /// a resumed suffix from a full retained-ring replacement.
    AgentStreamResume = 0x0000_0008,
    /// The server accepts the organization commands (`docs/organization.md`)
    /// and enforces every rule they carry: keyed sessions and
    /// resources, revisions, receipts, refusals, and the organization
    /// events. A client that does not see it treats organization as
    /// read-only.
    Organization = 0x0000_0010,
    /// The server accepts `SESSION_KILL` (`docs/organization.md`): one
    /// command ends every member of a session and deletes it. Advertised
    /// only beside [`Self::Organization`].
    SessionKill = 0x0000_0020,
    /// Dedicated SDK-owned `AgentSession` creation and typed native commands.
    NativeAgent = 0x0000_0040,
    /// The server accepts `PERF_CONTROL`: start and stop a span trace and
    /// switch heap accounting. A client MUST see the bit before sending the
    /// command.
    PerfControl = 0x0000_0080,
    /// The server accepts `SESSION_SET_PROJECT` (`docs/organization.md`):
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
    /// The server serves the `AccessibilityTree` resource kind: a
    /// producer-fed, replicated accessibility tree of an application.
    AccessibilityTree = 0x0000_8000,
    /// The server serves the `PixelSurface` resource kind: a producer-fed,
    /// replicated image of a window or display.
    PixelSurface = 0x0001_0000,
    /// The server serves the `Document` resource kind: a producer-fed,
    /// replicated document whose content type is fixed when it is opened.
    TypedDocument = 0x0002_0000,
    /// The server answers `MIRROR`: it lists the displays and windows it
    /// can mirror and starts or stops a mirror of one on demand.
    Mirrors = 0x0004_0000,
    /// The server answers the local-only `PRIVACY_STATUS`: the macOS
    /// privacy grants it holds.
    PrivacyStatus = 0x0008_0000,
    /// Read a durable resource record and its current activation without a live engine.
    DurableResourceInspection = 0x0010_0000,
    /// The server accepts `ARRANGEMENT_LOCK` (`docs/organization.md`): a
    /// locked group (a tab) refuses every close until it is unlocked, and
    /// arrangement documents carry the flag. Advertised only beside
    /// [`Self::Organization`]; a client MUST see the bit before sending the
    /// command.
    TabLock = 0x0020_0000,
    /// The server accepts `CLIPBOARD_REPLY`: a client's answer to a pane's
    /// OSC 52 clipboard read (`docs/program-sequences.md#clipboard-osc-52`).
    /// A client MUST see the bit first.
    ClipboardRead = 0x0040_0000,
    /// The server answers `HOST_CHECKLIST`: what works on its host, with
    /// reasons and fixes.
    HostChecklist = 0x0080_0000,
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
        Self::AccessibilityTree,
        Self::PixelSurface,
        Self::TypedDocument,
        Self::Mirrors,
        Self::PrivacyStatus,
        Self::DurableResourceInspection,
        Self::TabLock,
        Self::ClipboardRead,
        Self::HostChecklist,
    ];

    /// The `docs/protocol.md#handshake` constant, e.g. `"PATH_QUERY"`.
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
            Self::AccessibilityTree => "ACCESSIBILITY_TREE",
            Self::PixelSurface => "PIXEL_SURFACE",
            Self::TypedDocument => "TYPED_DOCUMENT",
            Self::Mirrors => "MIRRORS",
            Self::PrivacyStatus => "PRIVACY_STATUS",
            Self::DurableResourceInspection => "DURABLE_RESOURCE_INSPECTION",
            Self::TabLock => "TAB_LOCK",
            Self::ClipboardRead => "CLIPBOARD_READ",
            Self::HostChecklist => "HOST_CHECKLIST",
        }
    }

    /// The bit's value in `features_ext` (a mask, not an index).
    #[must_use]
    pub const fn mask(self) -> u32 {
        self as u32
    }
}
