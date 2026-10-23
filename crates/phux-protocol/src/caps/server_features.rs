// -----------------------------------------------------------------------------
// Layer / LayerSet — SPEC §6.2 conformance-tier bitset (ADR-0015).
// -----------------------------------------------------------------------------

/// A single conformance tier (SPEC §6.2 / §16).
///
/// L1 is always implied; the negotiated set is the intersection of both peers'
/// [`LayerSet`]s, and out-of-tier messages MUST surface as protocol errors
/// (§16.4, ADR-0015).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Layer {
    /// Terminal substrate. Always implemented; always implied.
    L1 = 0x01,
    /// Collection lifecycle (OPTIONAL). SPEC §7.3 / §11.L2.
    L2 = 0x02,
    /// Metadata storage (OPTIONAL). SPEC §7.4 / §11.L3.
    L3 = 0x04,
}

/// A bit-field of [`Layer`]s, one `u8` on the wire. L1 is always set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerSet(u8);

impl LayerSet {
    /// The L1-only set.
    #[must_use]
    pub const fn new() -> Self {
        Self(Layer::L1 as u8)
    }

    /// Build a set containing all listed layers (plus the always-on L1).
    #[must_use]
    pub const fn with(layers: &[Layer]) -> Self {
        let mut bits = Layer::L1 as u8;
        let mut i = 0;
        while i < layers.len() {
            bits |= layers[i] as u8;
            i += 1;
        }
        Self(bits)
    }

    /// The full set: L1 + L2 + L3.
    #[must_use]
    pub const fn all() -> Self {
        Self((Layer::L1 as u8) | (Layer::L2 as u8) | (Layer::L3 as u8))
    }

    /// Insert `layer` into the set. L1 cannot be removed.
    pub const fn insert(&mut self, layer: Layer) {
        self.0 |= layer as u8;
    }

    /// Test whether `layer` is in the set.
    #[must_use]
    pub const fn contains(self, layer: Layer) -> bool {
        self.0 & (layer as u8) != 0
    }

    /// Raw wire byte, L1 forced on.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self.0 | (Layer::L1 as u8)
    }

    /// Inverse of [`Self::as_wire`]: unknown bits dropped, L1 forced on.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Self {
        let known = (Layer::L1 as u8) | (Layer::L2 as u8) | (Layer::L3 as u8);
        Self((byte & known) | (Layer::L1 as u8))
    }
}

impl Default for LayerSet {
    fn default() -> Self {
        Self::new()
    }
}

// ServerFeature: one declarative list generates the consts, enum, mask, and
// names. Word 0 is closed (ADR-0137): no new bit at `0x8000_0000`, in the low
// gaps `0x1`..`0x8`, or at retired `0x1000`; the next feature goes in a
// trailing `features_ext` u32.

macro_rules! define_server_features {
    ($(
        $(#[$doc:meta])*
        $variant:ident = $const_name:ident = $bits:expr
    ),* $(,)?) => {
        $(
            #[doc = concat!("Wire bit for [`ServerFeature::", stringify!($variant), "`].")]
            pub const $const_name: u32 = $bits;
        )*

        /// An additive server-owned protocol feature.
        #[repr(u32)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum ServerFeature {
            $(
                $(#[$doc])*
                $variant = $const_name,
            )*
        }

        impl ServerFeature {
            /// Every known feature, in bit order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The `docs/spec/proto.md` §6.2 constant, e.g. `"ACKNOWLEDGED_INPUT"`.
            #[must_use]
            pub const fn wire_name(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($const_name),)*
                }
            }

            /// [`Self::wire_name`] lower-cased, as `phux status --json` and
            /// `phux --capabilities --json` print it.
            #[must_use]
            pub const fn snake_name(self) -> &'static str {
                match self {
                    $(Self::$variant => {
                        const WIRE: &str = stringify!($const_name);
                        const N: usize = WIRE.len();
                        const BYTES: &[u8] = &{
                            let src = WIRE.as_bytes();
                            let mut out = [0u8; N];
                            let mut i = 0;
                            while i < N {
                                out[i] = src[i].to_ascii_lowercase();
                                i += 1;
                            }
                            out
                        };
                        match core::str::from_utf8(BYTES) {
                            Ok(s) => s,
                            Err(_) => panic!("feature wire name is ASCII"),
                        }
                    })*
                }
            }
        }
    };
}

define_server_features! {
    /// The server accepts idempotent `Command::ApplyInput` batches.
    AcknowledgedInput = ACKNOWLEDGED_INPUT = 0x0000_0010,
    /// The server accepts sandboxed, chunked `Command::PutFile` uploads.
    FileUpload = FILE_UPLOAD = 0x0000_0020,
    /// The server accepts `MOVE_RESOURCE` (ADR-0056). A client MUST see the
    /// bit first: a 1.x server without it answers an uncorrelated
    /// `ERROR Unsupported` for the frame type and keeps the connection.
    MoveResource = MOVE_RESOURCE = 0x0000_0040,
    /// The server accepts opaque terminal-emulator replies for attached PTYs.
    TerminalReply = TERMINAL_REPLY = 0x0000_0080,
    /// The server accepts the local-only `SHUTDOWN`. A client MUST see the
    /// bit first: a 1.x server without it answers `COMMAND_RESULT
    /// Unsupported` and keeps running.
    Shutdown = SHUTDOWN = 0x0000_0100,
    /// The server honors `SPAWN_RESOURCE.initial_size`. Sending it
    /// unadvertised is safe (skipped by length); the bit tells a
    /// layout-owning client whether a follow-up `RESIZE_TERMINAL` is needed.
    SpawnInitialSize = SPAWN_INITIAL_SIZE = 0x0000_0200,
    /// The server accepts `REPORT_AGENT_STATE` hook evidence.
    ReportAgentState = REPORT_AGENT_STATE = 0x0000_0400,
    /// The server answers `GET_PERF` with JSON performance telemetry.
    GetPerf = GET_PERF = 0x0000_0800,
    /// The server answers `TRANSCRIBE` by transcribing a finished upload and
    /// pasting the text. `0x1000` below it is retired-unshipped (ADR-0116)
    /// and MUST NOT be reused without a version bump.
    Transcribe = TRANSCRIBE = 0x0000_2000,
    /// The server serves `AgentSession` resources as well as Terminals
    /// (spawn fields, `APPEND_RESOURCE_OUTPUT`, the JSONL codec, close
    /// reasons, snapshot facets). All shapes are skip-by-length additive; the
    /// bit tells a client the kind it asked for is the kind it got.
    ResourceKinds = RESOURCE_KINDS = 0x0000_4000,
    /// The server answers `LIST_DIRECTORY` for its own host
    /// (`docs/spec/L3.md` §4). A client MUST see the bit first: a 1.x
    /// server without it answers an uncorrelated `ERROR Unsupported` for the
    /// frame type, so no `DIRECTORY_LISTING` ever answers the request.
    ListDirectory = LIST_DIRECTORY = 0x0000_8000,
    /// `GET_STATE { SERVER }` carries the trailing host-session inventory:
    /// one row per federation satellite, empty on a non-hub. The bit lets a
    /// client read an empty list as "no satellites", not "an older hub".
    HostSessions = HOST_SESSIONS = 0x0001_0000,
    /// The server honors keep-empty sessions (ADR-0105). A client MUST see
    /// the bit before sending `empty: true`: an older server seeds a shell.
    KeepEmptySessions = KEEP_EMPTY_SESSIONS = 0x0002_0000,
    /// The server answers `GET_METADATA { Global, "phux.whoami/v1" }` with the
    /// asking connection's identity (`docs/spec/L3.md` §3.9, ADR-0106); the
    /// bit makes an absent value meaningful.
    Whoami = WHOAMI = 0x0004_0000,
    /// The server understands `LIST_DIRECTORY.host` (`docs/spec/L3.md` §4.1):
    /// a hub relays to the named satellite, anything else is refused by name.
    /// A client MUST see the bit before trusting a listing came from the host
    /// it named; an older server lists its own host.
    ListDirectoryHost = LIST_DIRECTORY_HOST = 0x0008_0000,
    /// The server honors HELLO field 9 `ssh_origin`, only from a same-uid
    /// Unix-socket peer, and only to relabel the whoami route `ssh-stdio`;
    /// auth is unchanged. The bit lets a whoami reader trust that `uds`
    /// means no bridge announced ssh (`docs/spec/L3.md` §3.9).
    SshOrigin = SSH_ORIGIN = 0x0010_0000,
    /// The server evaluates `KILL_RESOURCE_IF` (ADR-0109) and answers
    /// `bind_instance` spawns with the instance token. A client MUST see the
    /// bit before sending the command.
    ConditionalKill = CONDITIONAL_KILL = 0x0020_0000,
    /// The connection may use QUIC multi-stream (ADR-0115): one control
    /// stream plus one client-opened bidi stream per attached Terminal. A
    /// client MUST NOT open a second QUIC stream without it. QUIC-only.
    QuicStreams = QUIC_STREAMS = 0x0040_0000,
    /// The server accepts `OPEN_LISTENER` on its Unix socket (ADR-0120). A
    /// client MUST see the bit before sending the command.
    OpenListener = OPEN_LISTENER = 0x0080_0000,
    /// Events carry journal stamps, `SUBSCRIBE_EVENTS { after_seq }` replays,
    /// and losses surface as `journal_gap` / `source_gap` (ADR-0123). The bit
    /// is what makes a cursor, and the absence of a gap, meaningful.
    EventJournal = EVENT_JOURNAL = 0x0100_0000,
    /// The server honors `SPAWN_RESOURCE.retain_secs` (ADR-0124). A client
    /// MUST see the bit before relying on a retained exit.
    RetainOnExit = RETAIN_ON_EXIT = 0x0200_0000,
    /// The server honors `SPAWN_RESOURCE.idempotency_key` (ADR-0126). A
    /// client MUST see the bit before retrying a spawn blind.
    SpawnIdempotency = SPAWN_IDEMPOTENCY = 0x0400_0000,
    /// The server honors declared attach roles (ADR-0127). A client MUST see
    /// the bit before relying on `VIEWER` or a deliberate takeover.
    AttachRoles = ATTACH_ROLES = 0x0800_0000,
    /// The server accepts `CLOSE_TAB_RESOURCES`, which leaves a keep-empty
    /// session empty (ADR-0105, ADR-0114). A client MUST see the bit before
    /// sending the command.
    CloseTabResources = CLOSE_TAB_RESOURCES = 0x1000_0000,
    /// The server dedupes kills and signals by their trailing
    /// `operation_id`, and a hub forwards keyed operations and `APPLY_INPUT`
    /// with `INCARNATION_CHANGED` fencing. A client MUST see the bit before
    /// retrying a keyed command blind.
    KeyedSignal = KEYED_SIGNAL = 0x2000_0000,
    /// The server holds `?signal` commands for approval (ADR-0128). A client
    /// MUST see the bit before writing a decision: an older server stores the
    /// decide key as an ordinary value.
    Approvals = APPROVALS = 0x4000_0000,
}

/// Bit-field of additive server-owned protocol features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ServerFeatureSet(u32);

impl ServerFeatureSet {
    const KNOWN: u32 = {
        let mut bits = 0;
        let mut i = 0;
        while i < ServerFeature::ALL.len() {
            bits |= ServerFeature::ALL[i] as u32;
            i += 1;
        }
        bits
    };

    /// Empty set for servers that advertise no additive features.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Every known feature, transport-gated ones included; a server clears
    /// those per connection with [`Self::without`].
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Build a set containing all listed features.
    #[must_use]
    pub const fn with(features: &[ServerFeature]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < features.len() {
            bits |= features[i] as u32;
            i += 1;
        }
        Self(bits)
    }

    /// Copy of this set with `feature` cleared.
    #[must_use]
    pub const fn without(self, feature: ServerFeature) -> Self {
        Self(self.0 & !(feature as u32))
    }

    /// Test whether `feature` is advertised.
    #[must_use]
    pub const fn contains(self, feature: ServerFeature) -> bool {
        self.0 & (feature as u32) != 0
    }

    /// True when no feature bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Raw wire bits, with unknown bits excluded.
    #[must_use]
    pub const fn as_wire(self) -> u32 {
        self.0 & Self::KNOWN
    }

    /// Decode known feature bits while ignoring future unknown bits.
    #[must_use]
    pub const fn from_wire(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }

    /// Known features in this set, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = ServerFeature> {
        ServerFeature::ALL
            .iter()
            .copied()
            .filter(move |feature| self.contains(*feature))
    }
}

#[path = "server_features/extended.rs"]
mod extended;
pub use extended::*;
