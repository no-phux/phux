//! [`ServerFeatureExt`]: the bits of the trailing capability word.

define_server_feature_word! {
    /// Features in the second, trailing server-capability word (ADR-0137).
    ServerFeatureExt {
        /// Host-side file/directory browsing and recursive fuzzy path search.
        PathQuery = PATH_QUERY = 0x0000_0001,
        /// `RESIZE_TERMINAL` carries and applies an optional cell pixel size
        /// (fields 4/5, ADR-0145), so a client can size panes without a
        /// viewport vote.
        ResizeCellPx = RESIZE_CELL_PX = 0x0000_0002,
        /// A kill's reply follows a commit that is the whole visible teardown
        /// (`docs/protocol.md#commands-events-resources`): the next `GET_STATE` already omits the
        /// killed resources, so a client need not wait for their reap.
        CommittedTeardown = COMMITTED_TEARDOWN = 0x0000_0004,
        /// `ATTACH_RESOURCE` accepts an agent cursor and explicitly distinguishes
        /// a resumed suffix from a full retained-ring replacement.
        AgentStreamResume = AGENT_STREAM_RESUME = 0x0000_0008,
        /// The server accepts the organization commands (`docs/organization.md`)
        /// and enforces every rule they carry: keyed sessions and
        /// resources, revisions, receipts, refusals, and the organization
        /// events. A client that does not see it treats organization as
        /// read-only.
        Organization = ORGANIZATION = 0x0000_0010,
        /// The server accepts `SESSION_KILL` (`docs/organization.md`): one
        /// command ends every member of a session and deletes it. Advertised
        /// only beside [`Self::Organization`].
        SessionKill = SESSION_KILL = 0x0000_0020,
        /// Dedicated SDK-owned `AgentSession` creation and typed native commands.
        NativeAgent = NATIVE_AGENT = 0x0000_0040,
        /// The server accepts `PERF_CONTROL`: start and stop a span trace and
        /// switch heap accounting. A client MUST see the bit before sending the
        /// command.
        PerfControl = PERF_CONTROL = 0x0000_0080,
        /// The server accepts `SESSION_SET_PROJECT` (`docs/organization.md`):
        /// link a session into a named project after creation, or clear the
        /// link. Advertised only beside [`Self::Organization`]; a client MUST see
        /// the bit before sending the command.
        SessionProject = SESSION_PROJECT = 0x0000_0100,
        /// The server answers `GET_FILE`: bounded chunks of any host file the
        /// serving user can read, addressed to this server or a named satellite.
        FileDownload = FILE_DOWNLOAD = 0x0000_0200,
        /// The server answers `CHECKOUT`: typed Git reads and index writes on
        /// the checkout a named Terminal is in, resolved from the Terminal's
        /// live working directory.
        Checkout = CHECKOUT = 0x0000_0400,
        /// Executing-host native integration inventory and keyed launch.
        NativeAgentLaunch = NATIVE_AGENT_LAUNCH = 0x0000_0800,
        /// The server answers `SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`, and
        /// `SERVICE_HTTP_CLOSE`: HTTP exchanges with a named service on its host
        /// (`opencode`) or on a named satellite's, authorized by the connection.
        HostServiceHttp = HOST_SERVICE_HTTP = 0x0000_1000,
        /// Resource creation commits placement with membership and launch intent,
        /// and returns stable entry identity and current execution in its receipt.
        CreatePlacement = CREATE_PLACEMENT = 0x0000_2000,
        /// Connected-server public workflow description and nested schemas.
        ApiDescription = API_DESCRIPTION = 0x0000_4000,
        /// The server serves the `AccessibilityTree` resource kind: a
        /// producer-fed, replicated accessibility tree of an application.
        AccessibilityTree = ACCESSIBILITY_TREE = 0x0000_8000,
        /// The server serves the `PixelSurface` resource kind: a producer-fed,
        /// replicated image of a window or display.
        PixelSurface = PIXEL_SURFACE = 0x0001_0000,
        /// The server serves the `Document` resource kind: a producer-fed,
        /// replicated document whose content type is fixed when it is opened.
        TypedDocument = TYPED_DOCUMENT = 0x0002_0000,
        /// The server answers `MIRROR`: it lists the displays and windows it
        /// can mirror and starts or stops a mirror of one on demand.
        Mirrors = MIRRORS = 0x0004_0000,
        /// The server answers the local-only `PRIVACY_STATUS`: the macOS
        /// privacy grants it holds.
        PrivacyStatus = PRIVACY_STATUS = 0x0008_0000,
        /// Read a durable resource record and its current activation without a live engine.
        DurableResourceInspection = DURABLE_RESOURCE_INSPECTION = 0x0010_0000,
        /// The server accepts `ARRANGEMENT_LOCK` (`docs/organization.md`): a
        /// locked group (a tab) refuses every close until it is unlocked, and
        /// arrangement documents carry the flag. Advertised only beside
        /// [`Self::Organization`]; a client MUST see the bit before sending the
        /// command.
        TabLock = TAB_LOCK = 0x0020_0000,
        /// The server accepts `CLIPBOARD_REPLY`: a client's answer to a pane's
        /// OSC 52 clipboard read (`docs/program-sequences.md#clipboard-osc-52`).
        /// A client MUST see the bit first.
        ClipboardRead = CLIPBOARD_READ = 0x0040_0000,
        /// The server answers `HOST_CHECKLIST`: what works on its host, with
        /// reasons and fixes.
        HostChecklist = HOST_CHECKLIST = 0x0080_0000,
        /// Explicitly approved, ephemeral 1Password credential exchanges.
        Credentials = CREDENTIALS = 0x0100_0000,
        /// The server answers `FORWARD_OPEN`, `FORWARD_READ`, `FORWARD_WRITE`,
        /// and `FORWARD_CLOSE`: TCP connections it dials on its own loopback
        /// for the client (`docs/ssh.md#forwarding-a-port`). Not advertised when
        /// `[forward] tcp = false`.
        ForwardTcp = FORWARD_TCP = 0x0200_0000,
        /// The server answers `AGENT_OFFER` and `AGENT_REPLY`: it brokers
        /// ssh-agent requests from its panes to a client that offered its
        /// agent (`docs/ssh.md#ssh-agent`). Advertised only with
        /// `[forward] agent = true`.
        ForwardAgent = FORWARD_AGENT = 0x0400_0000,
        /// Arrangement leaves carry slot tabs and the server keeps them
        /// (`docs/organization.md#slot-tabs`): an `ARRANGEMENT_REPLACE` flagged
        /// `SLOT_TABS` sets them, and one that isn't keeps them as they stand.
        /// Advertised only beside [`Self::Organization`]; a client MUST see the
        /// bit before writing tabs, since an older server drops them.
        SlotTabs = SLOT_TABS = 0x0800_0000,
        /// Current question reads, exact structured responses, and receipt lookup.
        CurrentQuestions = CURRENT_QUESTIONS = 0x1000_0000,
        /// The server accepts `VIEWPORT_COLORS`: an attached client's new
        /// outer default colours become its panes' OSC 10/11 answers
        /// mid-attach. A client MUST see the bit before sending the frame.
        ViewportColors = VIEWPORT_COLORS = 0x2000_0000,
        /// The server answers `CHECKOUT` contract 2: the `show` operation, and
        /// the `applied` answer and `git_unavailable` refusal for a client whose
        /// command carries contract 2. Advertised only beside
        /// [`Self::Checkout`]; a client MUST see the bit before sending `show`.
        CheckoutV2 = CHECKOUT_V2 = 0x4000_0000,
    }
}

impl ServerFeatureExt {
    /// The bit's value in `features_ext` (a mask, not an index).
    #[must_use]
    pub const fn mask(self) -> u32 {
        self as u32
    }
}
