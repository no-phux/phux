//! Capability advertisements (SPEC §6.2).
//!
//! Capabilities live in HELLO and apply for the life of the connection. The
//! server rewrites outbound SGR bytes to the advertised [`ColorSupport`]
//! (ADR-0013).

/// A client's color tier (SPEC §6.2), most to least permissive. The server
/// rewrites outbound VT bytes to fit; `TrueColor` is the default so an
/// unadvertised client is never silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorSupport {
    /// 24-bit direct RGB; SGR truecolor is forwarded verbatim.
    #[default]
    TrueColor,
    /// xterm 256-color palette.
    Indexed256,
    /// 16 system colors only.
    Indexed16,
    /// Monochrome: SGR color sequences MUST be stripped.
    Mono,
}

impl ColorSupport {
    /// Wire tag. Stable within v0.x; new variants append.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::TrueColor => 0,
            Self::Indexed256 => 1,
            Self::Indexed16 => 2,
            Self::Mono => 3,
        }
    }

    /// Inverse of [`Self::as_wire`]; `None` for an unknown tag.
    #[must_use]
    pub const fn from_wire(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::TrueColor,
            1 => Self::Indexed256,
            2 => Self::Indexed16,
            3 => Self::Mono,
            _ => return None,
        })
    }
}

/// How the server emits terminal content to this consumer (SPEC §6.2).
///
/// Either the byte-faithful raw PTY broadcast, or a per-consumer synthesized
/// state-sync tick for agent / remote consumers that want a coherent grid
/// model. An unknown wire tag decodes as [`OutputMode::Raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OutputMode {
    /// Raw PTY byte broadcast: the default, human-TUI path.
    #[default]
    Raw,
    /// Per-consumer synthesized grid deltas with a monotonic `seq` (ADR-0018).
    StateSync,
}

impl OutputMode {
    /// Wire tag. Stable within v0.x; new variants append.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::Raw => 0,
            Self::StateSync => 1,
        }
    }

    /// Inverse of [`Self::as_wire`]; an unknown tag is [`OutputMode::Raw`].
    #[must_use]
    pub const fn from_wire(tag: u8) -> Self {
        match tag {
            1 => Self::StateSync,
            _ => Self::Raw,
        }
    }
}
// -----------------------------------------------------------------------------
// Native bootstrap negotiation — ADR-0070 / protocol 0.7.
// -----------------------------------------------------------------------------

/// Hard upper bound for one `BOOTSTRAP_CHUNK.payload`; leaves envelope
/// headroom below the 16 MiB frame cap.
pub const MAX_BOOTSTRAP_CHUNK_BYTES: u32 = 8 * 1024 * 1024;
/// Hard upper bound for one `HISTORY_PAGE.payload`.
pub const MAX_HISTORY_PAGE_BYTES: u32 = 8 * 1024 * 1024;
/// Default advertised maximum for one bootstrap chunk (256 KiB).
pub const DEFAULT_BOOTSTRAP_CHUNK_BYTES: u32 = 256 * 1024;
/// Default advertised maximum for one history page (1 MiB).
pub const DEFAULT_HISTORY_PAGE_BYTES: u32 = 1024 * 1024;

/// A frame-payload compression algorithm (`docs/spec/proto.md` §6.4).
///
/// Decode-invisible: `FRAME_COMPRESSED` inflates back to the exact inner
/// frame bytes, so native records stay byte-identical end to end (§6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Compression {
    /// No transform. Frames go on the wire exactly as encoded.
    #[default]
    None = 0,
    /// Raw DEFLATE (RFC 1951), as produced by `flate2`'s `miniz_oxide` backend.
    Deflate = 1,
}

impl Compression {
    /// The wire tag.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a wire tag; an unknown algorithm is [`Self::None`], so an
    /// out-of-contract peer fails on its first wrapped frame instead of being
    /// misread.
    #[must_use]
    pub const fn from_u8(tag: u8) -> Self {
        match tag {
            1 => Self::Deflate,
            _ => Self::None,
        }
    }
}

/// The set of compression algorithms a peer accepts (`HELLO` field 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CompressionSet(u8);

impl CompressionSet {
    /// Bit for [`Compression::Deflate`].
    pub const DEFLATE: u8 = 0x01;

    /// The empty set: accept no compressed frames.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// The set built from a raw bitset, unknown bits preserved but inert.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw bitset for the wire.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Everything this build can decode.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::DEFLATE)
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `algorithm` is in the set.
    #[must_use]
    pub const fn contains(self, algorithm: Compression) -> bool {
        match algorithm {
            Compression::None => true,
            Compression::Deflate => self.0 & Self::DEFLATE != 0,
        }
    }

    /// The algorithm a server picks for a client offering this set.
    #[must_use]
    pub const fn select(self) -> Compression {
        if self.contains(Compression::Deflate) {
            Compression::Deflate
        } else {
            Compression::None
        }
    }
}

/// Negotiated per-frame byte bounds for bootstrap and history payloads: the
/// per-axis minimum of both peers' values, each in `1..=` the hard cap. A
/// payload above the negotiated value MUST be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BootstrapLimits {
    max_chunk_bytes: u32,
    max_history_page_bytes: u32,
}

impl BootstrapLimits {
    /// Construct validated limits.
    #[must_use]
    pub const fn new(max_chunk_bytes: u32, max_history_page_bytes: u32) -> Option<Self> {
        if max_chunk_bytes == 0
            || max_chunk_bytes > MAX_BOOTSTRAP_CHUNK_BYTES
            || max_history_page_bytes == 0
            || max_history_page_bytes > MAX_HISTORY_PAGE_BYTES
        {
            return None;
        }
        Some(Self {
            max_chunk_bytes,
            max_history_page_bytes,
        })
    }

    /// Maximum bytes permitted in one `BOOTSTRAP_CHUNK.payload`.
    #[must_use]
    pub const fn max_chunk_bytes(self) -> u32 {
        self.max_chunk_bytes
    }

    /// Maximum bytes permitted in one `HISTORY_PAGE.payload`.
    #[must_use]
    pub const fn max_history_page_bytes(self) -> u32 {
        self.max_history_page_bytes
    }

    /// Intersect two advertisements by taking the lower bound on each axis.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            max_chunk_bytes: if self.max_chunk_bytes < other.max_chunk_bytes {
                self.max_chunk_bytes
            } else {
                other.max_chunk_bytes
            },
            max_history_page_bytes: if self.max_history_page_bytes < other.max_history_page_bytes {
                self.max_history_page_bytes
            } else {
                other.max_history_page_bytes
            },
        }
    }
}

impl Default for BootstrapLimits {
    fn default() -> Self {
        Self {
            max_chunk_bytes: DEFAULT_BOOTSTRAP_CHUNK_BYTES,
            max_history_page_bytes: DEFAULT_HISTORY_PAGE_BYTES,
        }
    }
}

/// A `u8` wire bit-set of `$item` flags whose `Default` is every known flag.
/// Unknown bits are dropped on decode.
macro_rules! u8_flag_set {
    ($(#[$doc:meta])* $name:ident of $item:ident { $($variant:ident),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(u8);

        impl $name {
            const KNOWN: u8 = 0 $(| ($item::$variant as u8))+;

            /// The empty set.
            #[must_use]
            pub const fn new() -> Self {
                Self(0)
            }

            /// Every known flag.
            #[must_use]
            pub const fn all() -> Self {
                Self(Self::KNOWN)
            }

            /// The set of `items`.
            #[must_use]
            pub const fn with(items: &[$item]) -> Self {
                let mut bits = 0;
                let mut i = 0;
                while i < items.len() {
                    bits |= items[i] as u8;
                    i += 1;
                }
                Self(bits)
            }

            /// Whether `item` is in the set.
            #[must_use]
            pub const fn contains(self, item: $item) -> bool {
                self.0 & (item as u8) != 0
            }

            /// Known wire bits.
            #[must_use]
            pub const fn as_wire(self) -> u8 {
                self.0 & Self::KNOWN
            }

            /// Decode known bits, ignoring future ones.
            #[must_use]
            pub const fn from_wire(bits: u8) -> Self {
                Self(bits & Self::KNOWN)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::all()
            }
        }
    };
}

/// One synchronization profile a peer can bootstrap.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BootstrapProfileKind {
    /// Exact libghostty checkpoint state, then byte-identical raw PTY output.
    /// Bit `0x01` is permanently retired (an incomplete early native contract).
    NativeState = 1 << 3,
    /// Server-synthesized VT bootstrap followed by raw compatibility output.
    SynthesizedVtRaw = 1 << 1,
    /// Server-synthesized VT bootstrap followed by `StateSync` output.
    SynthesizedVtStateSync = 1 << 2,
}

u8_flag_set! {
    /// Additive bit-set of synchronization profiles supported by a peer.
    BootstrapProfileSet of BootstrapProfileKind {
        NativeState,
        SynthesizedVtRaw,
        SynthesizedVtStateSync,
    }
}

/// An immutable libghostty checkpoint codec version: an exact capability,
/// never a range, so negotiation cannot infer compatibility.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum EngineCodec {
    /// libghostty terminal checkpoint format version 2.
    LibghosttyCheckpointV2 = 2,
    /// Official `GHOSTSNPv1` feed snapshot with a progressive READY boundary.
    /// Id `3` is the capability, not the envelope's internal version.
    LibghosttySnapshotV1 = 3,
}

impl EngineCodec {
    /// Exact checkpoint envelope version selected on the wire.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode an exact checkpoint version.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            2 => Some(Self::LibghosttyCheckpointV2),
            3 => Some(Self::LibghosttySnapshotV1),
            _ => None,
        }
    }
}
/// Concrete encoding carried by one bootstrap stream (distinct from the
/// negotiated [`BootstrapProfile`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BootstrapCodec {
    /// Protocol-defined synthesized VT replay grammar version 1.
    SynthesizedVtV1,
    /// Exact libghostty checkpoint grammar.
    Native(EngineCodec),
    /// Newline-delimited JSON agent-event records v1: the `AgentSession`
    /// codec. Live output carries whole records and is always raw
    /// (`FRAME_ACK` forbidden). Gated on `ServerFeature::ResourceKinds`.
    AgentEventsJsonlV1,
}

impl BootstrapCodec {
    /// Wire tag for synthesized VT v1.
    pub const SYNTHESIZED_VT_V1_TAG: u8 = 0;
    /// Wire tag for a native engine codec followed by its exact version byte.
    pub const NATIVE_TAG: u8 = 1;
    /// Wire tag for agent-event JSONL v1. Tag `2` is skipped so the tag
    /// space never collides with the `EngineCodec` version byte that follows
    /// [`Self::NATIVE_TAG`] in a hex dump.
    pub const AGENT_EVENTS_JSONL_V1_TAG: u8 = 3;
}

/// Additive set of exact native engine codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EngineCodecSet(u64);

impl EngineCodecSet {
    const V2_BIT: u64 = 1 << 2;
    const SNAPSHOT_V1_BIT: u64 = 1 << 3;
    const KNOWN: u64 = Self::V2_BIT | Self::SNAPSHOT_V1_BIT;

    /// Empty codec set.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// All exact native codecs implemented by protocol 0.7.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Build a set from exact codec versions.
    #[must_use]
    pub const fn with(codecs: &[EngineCodec]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < codecs.len() {
            bits |= 1u64 << (codecs[index] as u8);
            index += 1;
        }
        Self(bits)
    }

    /// Whether this set contains an exact codec.
    #[must_use]
    pub const fn contains(self, codec: EngineCodec) -> bool {
        self.0 & (1u64 << (codec as u8)) != 0
    }

    /// Known wire bits.
    #[must_use]
    pub const fn as_wire(self) -> u64 {
        self.0 & Self::KNOWN
    }

    /// Decode known bits and ignore future bits.
    #[must_use]
    pub const fn from_wire(bits: u64) -> Self {
        Self(bits & Self::KNOWN)
    }

    /// Highest exact codec shared by both sets.
    #[must_use]
    pub const fn highest_common(self, other: Self) -> Option<EngineCodec> {
        if self.0 & other.0 & Self::SNAPSHOT_V1_BIT != 0 {
            Some(EngineCodec::LibghosttySnapshotV1)
        } else if self.0 & other.0 & Self::V2_BIT != 0 {
            Some(EngineCodec::LibghosttyCheckpointV2)
        } else {
            None
        }
    }
}

/// libghostty checkpoint capabilities required by native synchronization.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EngineFeature {
    /// Parser continuation state is serialized.
    Continuation = 1 << 0,
    /// The codec exposes an incremental READY publication boundary.
    ReadyBoundary = 1 << 1,
    /// History can continue in independently delivered pages after READY.
    HistoryPages = 1 << 2,
    /// Bounded history requests, sequenced pages, and cursor-scoped statuses.
    BoundedHistoryControl = 1 << 3,
}

/// Additive set of libghostty checkpoint features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EngineFeatureSet(u32);

impl EngineFeatureSet {
    const KNOWN: u32 = (EngineFeature::Continuation as u32)
        | (EngineFeature::ReadyBoundary as u32)
        | (EngineFeature::HistoryPages as u32)
        | (EngineFeature::BoundedHistoryControl as u32);

    /// Empty feature set.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// All features required by the protocol-0.7 native profile.
    #[must_use]
    pub const fn required_native() -> Self {
        Self(Self::KNOWN)
    }

    /// Build a feature set.
    #[must_use]
    pub const fn with(features: &[EngineFeature]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < features.len() {
            bits |= features[index] as u32;
            index += 1;
        }
        Self(bits)
    }

    /// Whether `feature` is present.
    #[must_use]
    pub const fn contains(self, feature: EngineFeature) -> bool {
        self.0 & (feature as u32) != 0
    }

    /// Whether every native-required feature is present.
    #[must_use]
    pub const fn supports_native(self) -> bool {
        self.0 & Self::KNOWN == Self::KNOWN
    }

    /// Known wire bits.
    #[must_use]
    pub const fn as_wire(self) -> u32 {
        self.0 & Self::KNOWN
    }

    /// Decode known bits and ignore future bits.
    #[must_use]
    pub const fn from_wire(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }

    /// Feature intersection.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self((self.0 & other.0) & Self::KNOWN)
    }
}

/// Bootstrap negotiation capabilities advertised by a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BootstrapCapabilities {
    /// Supported explicit synchronization profiles.
    pub profiles: BootstrapProfileSet,
    /// Supported exact libghostty checkpoint versions for `NativeState`.
    pub native_codecs: EngineCodecSet,
    /// Supported libghostty checkpoint features for `NativeState`.
    pub native_features: EngineFeatureSet,
    /// Maximum payload bounds this peer accepts.
    pub limits: BootstrapLimits,
}

impl BootstrapCapabilities {
    /// Synthesized VT compatibility profiles only; a native host opts in
    /// through [`Self::with_native`] after probing its engine.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            profiles: BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
                BootstrapProfileKind::SynthesizedVtStateSync,
            ]),
            native_codecs: EngineCodecSet::new(),
            native_features: EngineFeatureSet::new(),
            limits: BootstrapLimits {
                max_chunk_bytes: DEFAULT_BOOTSTRAP_CHUNK_BYTES,
                max_history_page_bytes: DEFAULT_HISTORY_PAGE_BYTES,
            },
        }
    }

    /// Advertise one exact native codec. Native support is indivisible: a
    /// partial feature set removes any native advertisement instead.
    #[must_use]
    pub const fn with_native(mut self, codec: EngineCodec, features: EngineFeatureSet) -> Self {
        if !features.supports_native() {
            self.profiles = BootstrapProfileSet::from_wire(
                self.profiles.as_wire() & !(BootstrapProfileKind::NativeState as u8),
            );
            self.native_codecs = EngineCodecSet::new();
            self.native_features = EngineFeatureSet::new();
            return self;
        }

        self.profiles = BootstrapProfileSet::from_wire(
            self.profiles.as_wire() | BootstrapProfileKind::NativeState as u8,
        );
        self.native_codecs = EngineCodecSet::with(&[codec]);
        self.native_features = EngineFeatureSet::required_native();
        self
    }

    /// Replace the profile set.
    #[must_use]
    pub const fn with_profiles(mut self, profiles: BootstrapProfileSet) -> Self {
        self.profiles = profiles;
        self
    }

    /// Replace the exact native codec set.
    #[must_use]
    pub const fn with_native_codecs(mut self, codecs: EngineCodecSet) -> Self {
        self.native_codecs = codecs;
        self
    }

    /// Replace the native feature set.
    #[must_use]
    pub const fn with_native_features(mut self, features: EngineFeatureSet) -> Self {
        self.native_features = features;
        self
    }

    /// Replace payload limits.
    #[must_use]
    pub const fn with_limits(mut self, limits: BootstrapLimits) -> Self {
        self.limits = limits;
        self
    }
}

impl Default for BootstrapCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

/// The exact synchronization profile selected in `HELLO_OK`. The variants
/// are the whole legal matrix; `NativeState + StateSync` is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BootstrapProfile {
    /// Exact libghostty engine checkpoint plus raw live bytes.
    NativeState {
        /// Exact immutable checkpoint format.
        codec: EngineCodec,
        /// Negotiated feature intersection; includes all native-required bits.
        features: EngineFeatureSet,
    },
    /// Synthesized VT bootstrap plus raw compatibility output.
    SynthesizedVtRaw,
    /// Synthesized VT bootstrap plus `StateSync` output.
    SynthesizedVtStateSync,
}

impl BootstrapProfile {
    /// Wire tag for bounded-history `NativeState`. Tag `0` is permanently
    /// retired with the legacy native profile.
    pub const NATIVE_STATE_TAG: u8 = 3;
    /// Wire tag for `SynthesizedVtRaw`.
    pub const SYNTHESIZED_VT_RAW_TAG: u8 = 1;
    /// Wire tag for `SynthesizedVtStateSync`.
    pub const SYNTHESIZED_VT_STATE_SYNC_TAG: u8 = 2;
}

/// Failure to select an explicit synchronization profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecUnavailable;

/// Per-stream profile repeated in `BOOTSTRAP_BEGIN`: the connection's
/// [`BootstrapProfile`] for a Terminal stream, or the kind-fixed codec of
/// another resource kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BootstrapStreamProfile {
    /// Exact native checkpoint followed by raw PTY bytes.
    NativeState {
        /// Exact native checkpoint grammar carried by the stream.
        codec: EngineCodec,
    },
    /// Synthesized VT bootstrap followed by raw compatibility bytes.
    SynthesizedVtRaw,
    /// Synthesized VT bootstrap followed by `StateSync` bytes.
    SynthesizedVtStateSync,
    /// Agent-event JSONL records, raw live: every `AgentSession` stream.
    AgentEventsJsonlV1,
}

/// Select one explicit profile and the negotiated payload bounds.
///
/// Native is preferred when both peers advertise it with a common codec and
/// every required feature. Otherwise the client's output mode's synthesized
/// profile, then the other one, each only when both peers advertise it.
pub fn select_bootstrap_profile(
    client: &ClientCapabilities,
    server: &BootstrapCapabilities,
) -> Result<(BootstrapProfile, BootstrapLimits), CodecUnavailable> {
    let limits = client.bootstrap.limits.intersect(server.limits);
    if client
        .bootstrap
        .profiles
        .contains(BootstrapProfileKind::NativeState)
        && server.profiles.contains(BootstrapProfileKind::NativeState)
        && let Some(codec) = client
            .bootstrap
            .native_codecs
            .highest_common(server.native_codecs)
    {
        let features = client
            .bootstrap
            .native_features
            .intersect(server.native_features);
        if features.supports_native() {
            return Ok((BootstrapProfile::NativeState { codec, features }, limits));
        }
    }

    let raw = (
        BootstrapProfileKind::SynthesizedVtRaw,
        BootstrapProfile::SynthesizedVtRaw,
    );
    let state_sync = (
        BootstrapProfileKind::SynthesizedVtStateSync,
        BootstrapProfile::SynthesizedVtStateSync,
    );
    let order = match client.output_mode {
        OutputMode::Raw => [raw, state_sync],
        OutputMode::StateSync => [state_sync, raw],
    };
    order
        .into_iter()
        .find(|(kind, _)| {
            client.bootstrap.profiles.contains(*kind) && server.profiles.contains(*kind)
        })
        .map(|(_, profile)| (profile, limits))
        .ok_or(CodecUnavailable)
}

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
    /// bit first: an older server drops the unknown frame silently.
    MoveResource = MOVE_RESOURCE = 0x0000_0040,
    /// The server accepts opaque terminal-emulator replies for attached PTYs.
    TerminalReply = TERMINAL_REPLY = 0x0000_0080,
    /// The server accepts the local-only `SHUTDOWN`. A client MUST see the
    /// bit first: an older server drops the tag, indistinguishable from a
    /// stop that did not happen.
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
    /// (`docs/spec/L3.md` §4). A client MUST see the bit first, or the
    /// request waits forever on an older server.
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

/// One image-transport protocol the client may advertise (SPEC §6.2).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageProtocol {
    /// VT340 sixel graphics, transported via DCS.
    Sixel = 1 << 0,
    /// Kitty graphics protocol, transported via APC `G` payloads.
    KittyGraphics = 1 << 1,
    /// iTerm2 inline images, transported via OSC 1337.
    Iterm2 = 1 << 2,
}

u8_flag_set! {
    /// A bit-field of [`ImageProtocol`]s.
    ImageProtocolSet of ImageProtocol { Sixel, KittyGraphics, Iterm2 }
}

/// One keyboard protocol the client may advertise (SPEC §6.2).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyboardProtocol {
    /// Kitty keyboard protocol APC replies.
    Kitty = 1 << 0,
    /// xterm modifyOtherKeys-style replies.
    ModifyOtherKeys = 1 << 1,
}

u8_flag_set! {
    /// A bit-field of [`KeyboardProtocol`]s.
    KeyboardProtocolSet of KeyboardProtocol { Kitty, ModifyOtherKeys }
}

impl KeyboardProtocolSet {
    /// True when no keyboard protocol is advertised.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// The client's advertised capability set (SPEC §6.2). Construct with
/// [`Self::new`] and the `with_*` setters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ClientCapabilities {
    /// The client's color tier.
    pub color_support: ColorSupport,
    /// The conformance tiers the client speaks (SPEC §6.2 / §16).
    pub layers: LayerSet,
    /// Image protocols the client can render.
    pub image_protocols: ImageProtocolSet,
    /// Keyboard extension protocols the client understands.
    pub kbd_protocols: KeyboardProtocolSet,
    /// Whether OSC 8 hyperlink framing may be forwarded to the client.
    pub hyperlinks: bool,
    /// Preferred compatibility emitter when native is not selected.
    pub output_mode: OutputMode,
    /// The outer terminal's default colors (OSC 10/11), installed on the
    /// server's emulator so OSC queries inside phux answer as they would
    /// outside. `None` for non-TTY and older clients.
    pub default_colors: Option<TerminalDefaultColors>,
    /// Explicit bootstrap profiles, exact native codecs/features, and receive bounds.
    pub bootstrap: BootstrapCapabilities,
    /// Frame compressions this client can inflate (`docs/spec/proto.md`
    /// §6.4). Empty is right for a Unix socket.
    pub compression: CompressionSet,
    /// The ssh endpoints `phux stdio-bridge` stamps on the HELLO it relays
    /// (HELLO field 9, `docs/spec/L3.md` §3.9).
    pub ssh_origin: Option<crate::wire::ssh_origin::SshOrigin>,
    /// Whether this client can demultiplex per-Terminal QUIC streams. An
    /// explicit offer, never inferred from the transport.
    pub quic_streams: bool,
}

/// Effective default colors reported by the client's outer terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalDefaultColors {
    /// Effective default foreground (OSC 10).
    pub foreground: TerminalColor,
    /// Effective default background (OSC 11).
    pub background: TerminalColor,
}

/// A 24-bit RGB color carried in terminal capability negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TerminalColor {
    /// Red component.
    pub r: u8,
    /// Green component.
    pub g: u8,
    /// Blue component.
    pub b: u8,
}

impl ClientCapabilities {
    /// The default capability set: truecolor, L1 only, raw output.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            color_support: ColorSupport::TrueColor,
            layers: LayerSet::new(),
            image_protocols: ImageProtocolSet::all(),
            kbd_protocols: KeyboardProtocolSet::all(),
            hyperlinks: true,
            output_mode: OutputMode::Raw,
            default_colors: None,
            bootstrap: BootstrapCapabilities::new(),
            compression: CompressionSet::new(),
            ssh_origin: None,
            quic_streams: false,
        }
    }

    /// Builder setter for [`Self::compression`].
    #[must_use]
    pub const fn with_compression(mut self, compression: CompressionSet) -> Self {
        self.compression = compression;
        self
    }

    /// Builder setter for [`Self::ssh_origin`].
    #[must_use]
    pub const fn with_ssh_origin(mut self, origin: crate::wire::ssh_origin::SshOrigin) -> Self {
        self.ssh_origin = Some(origin);
        self
    }

    /// Builder setter for [`Self::quic_streams`].
    #[must_use]
    pub const fn with_quic_streams(mut self, enabled: bool) -> Self {
        self.quic_streams = enabled;
        self
    }

    /// Builder setter for [`Self::output_mode`].
    #[must_use]
    pub const fn with_output_mode(mut self, output_mode: OutputMode) -> Self {
        self.output_mode = output_mode;
        self
    }

    /// Builder setter for [`Self::default_colors`].
    #[must_use]
    pub const fn with_default_colors(mut self, colors: TerminalDefaultColors) -> Self {
        self.default_colors = Some(colors);
        self
    }
    /// Builder setter for [`Self::bootstrap`].
    #[must_use]
    pub const fn with_bootstrap(mut self, bootstrap: BootstrapCapabilities) -> Self {
        self.bootstrap = bootstrap;
        self
    }

    /// Builder setter for [`Self::color_support`].
    #[must_use]
    pub const fn with_color_support(mut self, color_support: ColorSupport) -> Self {
        self.color_support = color_support;
        self
    }

    /// Builder setter for [`Self::layers`].
    #[must_use]
    pub const fn with_layers(mut self, layers: LayerSet) -> Self {
        self.layers = layers;
        self
    }

    /// Builder setter for [`Self::image_protocols`].
    #[must_use]
    pub const fn with_image_protocols(mut self, image_protocols: ImageProtocolSet) -> Self {
        self.image_protocols = image_protocols;
        self
    }

    /// Builder setter for [`Self::kbd_protocols`].
    #[must_use]
    pub const fn with_kbd_protocols(mut self, kbd_protocols: KeyboardProtocolSet) -> Self {
        self.kbd_protocols = kbd_protocols;
        self
    }

    /// Builder setter for [`Self::hyperlinks`].
    #[must_use]
    pub const fn with_hyperlinks(mut self, hyperlinks: bool) -> Self {
        self.hyperlinks = hyperlinks;
        self
    }
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

/// What the server implements, advertised in `HELLO_OK` (SPEC §6.1). New
/// server-owned capabilities append as trailing fields; the next is a second
/// feature word (ADR-0137).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerCapabilities {
    /// The conformance tiers the server mounts (SPEC §6.2 / §16).
    pub layers: LayerSet,
    /// Additive server-owned protocol features.
    pub features: ServerFeatureSet,
    /// The frame compression this server selected for the connection
    /// (`docs/spec/proto.md` §6.4). Always one the client offered.
    pub compression: Compression,
}

impl ServerCapabilities {
    /// The default server capability set: L1 only, no features.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            layers: LayerSet::new(),
            features: ServerFeatureSet::new(),
            compression: Compression::None,
        }
    }

    /// Builder setter for [`Self::compression`].
    #[must_use]
    pub const fn with_compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// Builder setter for [`Self::layers`].
    #[must_use]
    pub const fn with_layers(mut self, layers: LayerSet) -> Self {
        self.layers = layers;
        self
    }

    /// Builder setter for [`Self::features`].
    #[must_use]
    pub const fn with_features(mut self, features: ServerFeatureSet) -> Self {
        self.features = features;
        self
    }
}

impl Default for ServerCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect the client terminal's color tier from environment hints.
///
/// `$COLORTERM`, then `$TERM` (suffixes; `dumb` is mono), then
/// `$TERM_PROGRAM`, falling back to truecolor: an over-claim is recoverable,
/// an under-claim silently degrades.
#[must_use]
pub fn detect_color_support() -> ColorSupport {
    detect_from_env(|key| std::env::var(key).ok())
}

/// [`detect_color_support`] over an injected environment lookup.
fn detect_from_env<F>(env: F) -> ColorSupport
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(ct) = env("COLORTERM") {
        let ct_lc = ct.to_ascii_lowercase();
        if ct_lc == "truecolor" || ct_lc == "24bit" {
            return ColorSupport::TrueColor;
        }
    }

    let term = env("TERM").unwrap_or_default();
    let term_lc = term.to_ascii_lowercase();
    if term_lc.ends_with("-direct") || term_lc.ends_with("-truecolor") {
        return ColorSupport::TrueColor;
    }
    if term_lc.ends_with("-256color") {
        return ColorSupport::Indexed256;
    }
    if !term_lc.is_empty() && !term_lc.contains("color") {
        // `xterm`, `linux`, `vt100`: anything richer carries a suffix.
        if term_lc == "dumb" {
            return ColorSupport::Mono;
        }
        return ColorSupport::Indexed16;
    }

    if let Some(tp) = env("TERM_PROGRAM") {
        let tp_lc = tp.to_ascii_lowercase();
        if tp_lc == "iterm.app" || tp_lc == "wezterm" {
            return ColorSupport::TrueColor;
        }
        if tp_lc == "apple_terminal" {
            return ColorSupport::Indexed256;
        }
    }

    ColorSupport::TrueColor
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn color_support_wire_roundtrips_every_variant() {
        for v in [
            ColorSupport::TrueColor,
            ColorSupport::Indexed256,
            ColorSupport::Indexed16,
            ColorSupport::Mono,
        ] {
            let tag = v.as_wire();
            let back = ColorSupport::from_wire(tag).expect("known tag");
            assert_eq!(back, v);
        }
        assert!(ColorSupport::from_wire(0xFF).is_none());
    }

    #[test]
    fn protocol_sets_ignore_unknown_bits() {
        assert_eq!(ImageProtocolSet::from_wire(0xFF), ImageProtocolSet::all());
        assert_eq!(ImageProtocolSet::all().as_wire(), 0x07);
        assert_eq!(
            KeyboardProtocolSet::from_wire(0xFF),
            KeyboardProtocolSet::all()
        );
        assert_eq!(KeyboardProtocolSet::all().as_wire(), 0x03);
    }

    #[test]
    fn color_support_detection_from_env() {
        let cases: &[(&[(&str, &str)], ColorSupport)] = &[
            (
                &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
                ColorSupport::TrueColor,
            ),
            (
                &[("COLORTERM", "24bit"), ("TERM", "xterm")],
                ColorSupport::TrueColor,
            ),
            (&[("TERM", "xterm-256color")], ColorSupport::Indexed256),
            (&[("TERM", "xterm-direct")], ColorSupport::TrueColor),
            (&[("TERM", "xterm")], ColorSupport::Indexed16),
            (&[("TERM", "dumb")], ColorSupport::Mono),
            (
                &[("TERM_PROGRAM", "Apple_Terminal")],
                ColorSupport::Indexed256,
            ),
            (&[("TERM_PROGRAM", "iTerm.app")], ColorSupport::TrueColor),
            (&[], ColorSupport::TrueColor),
        ];
        for (pairs, want) in cases {
            assert_eq!(detect_from_env(env_map(pairs)), *want, "{pairs:?}");
        }
    }

    #[test]
    fn native_advertisement_is_opt_in_and_indivisible() {
        let synth = BootstrapProfileSet::with(&[
            BootstrapProfileKind::SynthesizedVtRaw,
            BootstrapProfileKind::SynthesizedVtStateSync,
        ]);
        let base = BootstrapCapabilities::new();
        assert_eq!(base.profiles, synth);
        assert_eq!(base.native_codecs.as_wire(), 0);
        assert_eq!(base.native_features.as_wire(), 0);

        let native = base.with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        );
        assert!(native.profiles.contains(BootstrapProfileKind::NativeState));
        assert!(
            native
                .profiles
                .contains(BootstrapProfileKind::SynthesizedVtRaw)
        );
        assert!(
            native
                .native_codecs
                .contains(EngineCodec::LibghosttyCheckpointV2)
        );
        assert_eq!(native.native_features.as_wire(), 0x0000_000f);

        let partial =
            EngineFeatureSet::with(&[EngineFeature::Continuation, EngineFeature::ReadyBoundary]);
        let withdrawn = native.with_native(EngineCodec::LibghosttyCheckpointV2, partial);
        assert_eq!(withdrawn, base);
    }

    /// Every variant has exactly one name, each bit is unique, the known
    /// mask is their union, and each bit appears in `docs/spec/proto.md`
    /// §6.2 by name and value in both the bitset block and the
    /// `ServerCapabilities` sentence.
    #[test]
    fn every_server_feature_has_one_name_and_a_unique_bit() {
        let proto = include_str!("../../../docs/spec/proto.md");
        let mut union = 0_u32;
        let mut names = Vec::new();
        let mut snakes = Vec::new();
        for &feature in ServerFeature::ALL {
            let name = feature.wire_name();
            let snake = feature.snake_name();
            let bits = feature as u32;
            assert_eq!(bits.count_ones(), 1, "{name} must be a single bit");
            assert_eq!(union & bits, 0, "{name} reuses an allocated bit");
            union |= bits;
            assert_eq!(
                snake,
                name.to_ascii_lowercase(),
                "{name} snake_case is not its constant lower-cased"
            );
            assert!(!names.contains(&name), "{name} is declared more than once");
            assert!(
                !snakes.contains(&snake),
                "{snake} is declared more than once"
            );
            names.push(name);
            snakes.push(snake);
            let block = format!("{name} ");
            let block_value = format!("0x{bits:08X}");
            let prose = format!("`{name} = 0x{bits:X}`");
            assert!(
                proto
                    .lines()
                    .any(|l| l.trim_start().starts_with(&block) && l.contains(&block_value)),
                "proto.md §6.2 bitset block lacks `{name} = {block_value}`"
            );
            assert!(
                proto.replace('\n', " ").contains(&prose),
                "proto.md §6.2 ServerCapabilities sentence lacks {prose}"
            );
        }
        assert_eq!(names.len(), ServerFeature::ALL.len());
        assert_eq!(snakes.len(), ServerFeature::ALL.len());
        assert_eq!(ServerFeatureSet::all().as_wire(), union);
        assert_eq!(ServerFeatureSet::from_wire(u32::MAX).as_wire(), union);
        // ADR-0137: word 0 stays closed. These masks are the inventory of
        // holes (never assigned, retired-unshipped, reserved-unallocated).
        assert_eq!(union & 0x0000_000F, 0, "low nibble stays unassigned");
        assert_eq!(union & 0x0000_1000, 0, "0x1000 stays retired-unshipped");
        assert_eq!(union & 0x8000_0000, 0, "0x80000000 stays unallocated");
    }
}
