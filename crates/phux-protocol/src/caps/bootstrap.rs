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
    /// Official `GHOSTSNPv1` feed snapshot with a progressive READY boundary
    /// and empty READY/FINISH markers. Id `4` is the capability, not the
    /// envelope's internal version.
    ///
    /// Id `3` is permanently retired: it named the earlier `GHOSTSNPv1`
    /// bytes whose READY/FINISH carried BLAKE3 digests. Neither engine
    /// decodes the other's bytes, so id `3` is never advertised and is
    /// ignored on receipt.
    LibghosttySnapshotV1 = 4,
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
            4 => Some(Self::LibghosttySnapshotV1),
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
    const SNAPSHOT_V1_BIT: u64 = 1 << 4;
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
    /// Full retained agent-event JSONL replacement, followed by raw live records.
    AgentEventsJsonlV1,
    /// Agent-event JSONL suffix appended after an already committed record.
    /// The server advertises `AgentStreamResume` before accepting a cursor.
    AgentEventsJsonlResumeV1 {
        /// Last record already committed by the consumer.
        after_seq: u64,
    },
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
