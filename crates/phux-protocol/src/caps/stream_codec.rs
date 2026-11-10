/// Concrete encoding carried by one bootstrap stream (distinct from the
/// negotiated [`BootstrapProfile`]).
///
/// Which kind streams a codec, and what that stream may carry, is that
/// kind's catalog row ([`crate::kinds::StreamContract`]).
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
    /// One JSON document per message, the whole accessibility tree v1: the
    /// `AccessibilityTree` codec. The bootstrap carries the newest snapshot
    /// and every live message replaces it. Gated on
    /// `ServerFeatureExt::AccessibilityTree`.
    AccessibilityTreeJsonV1,
    /// One PNG or JPEG image per message, told apart by its signature (the
    /// name predates JPEG), the whole pixel surface v1: the
    /// `PixelSurface` codec. The bootstrap carries the newest image and
    /// every live message replaces it. Gated on
    /// `ServerFeatureExt::PixelSurface`.
    PixelSurfacePngV1,
    /// A content-typed document v1: the `Document` codec. The bootstrap
    /// payload is the resource's content type behind a one-byte length,
    /// then the newest document; every live message is a whole document.
    /// Gated on `ServerFeatureExt::TypedDocument`.
    DocumentTypedV1,
    /// A codec some catalog kind streams that has no variant of its own: a
    /// kind added with only its catalog row. Never carries a tag that has
    /// one.
    Kind {
        /// The codec's wire tag.
        tag: u8,
    },
    /// A codec tag this build does not know, from a newer server. Only
    /// the tag is kept; any bytes after it in the field are skipped. Only
    /// decoding builds one, and never for an allocated tag
    /// ([`Self::is_allocated_tag`]).
    Unknown {
        /// The unrecognised codec tag.
        tag: u8,
    },
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
    /// Wire tag for accessibility-tree JSON v1.
    pub const ACCESSIBILITY_TREE_JSON_V1_TAG: u8 = 4;
    /// Wire tag for pixel-surface PNG v1.
    pub const PIXEL_SURFACE_PNG_V1_TAG: u8 = 5;
    /// Wire tag for typed document v1.
    pub const DOCUMENT_TYPED_V1_TAG: u8 = 6;

    /// Whether `tag` names a codec some catalog kind streams, which
    /// [`Self::Unknown`] must never carry.
    #[must_use]
    pub const fn is_allocated_tag(tag: u8) -> bool {
        crate::kinds::codec_kind(tag).is_some()
    }

    /// The codec's wire tag (a native codec's version byte follows it).
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::SynthesizedVtV1 => Self::SYNTHESIZED_VT_V1_TAG,
            Self::Native(_) => Self::NATIVE_TAG,
            Self::AgentEventsJsonlV1 => Self::AGENT_EVENTS_JSONL_V1_TAG,
            Self::AccessibilityTreeJsonV1 => Self::ACCESSIBILITY_TREE_JSON_V1_TAG,
            Self::PixelSurfacePngV1 => Self::PIXEL_SURFACE_PNG_V1_TAG,
            Self::DocumentTypedV1 => Self::DOCUMENT_TYPED_V1_TAG,
            Self::Kind { tag } | Self::Unknown { tag } => tag,
        }
    }

    /// The codec `tag` names under `catalog`, for any tag but
    /// [`Self::NATIVE_TAG`], whose version byte the caller reads.
    #[must_use]
    pub const fn from_tag_in(catalog: &'static [crate::kinds::KindSpec], tag: u8) -> Self {
        match tag {
            Self::SYNTHESIZED_VT_V1_TAG => Self::SynthesizedVtV1,
            Self::AGENT_EVENTS_JSONL_V1_TAG => Self::AgentEventsJsonlV1,
            Self::ACCESSIBILITY_TREE_JSON_V1_TAG => Self::AccessibilityTreeJsonV1,
            Self::PIXEL_SURFACE_PNG_V1_TAG => Self::PixelSurfacePngV1,
            Self::DOCUMENT_TYPED_V1_TAG => Self::DocumentTypedV1,
            tag if crate::kinds::codec_kind_in(catalog, tag).is_some() => Self::Kind { tag },
            tag => Self::Unknown { tag },
        }
    }

    /// [`Self::from_tag_in`] this build's catalog.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Self {
        Self::from_tag_in(&crate::kinds::KINDS, tag)
    }
}

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
    /// The newest whole accessibility tree as one JSON document, followed
    /// by raw live replacements, each a whole document.
    AccessibilityTreeJsonV1,
    /// The newest whole pixel surface as one PNG or JPEG image, followed by raw
    /// live replacements, each a whole image.
    PixelSurfacePngV1,
    /// The resource's content type and newest document in one bootstrap
    /// ([`crate::wire::frame::typed_document`]), followed by raw live
    /// replacements, each a whole document of that type.
    DocumentTypedV1,
    /// The raw stream of a catalog kind's codec that has no variant of its
    /// own ([`BootstrapCodec::Kind`]).
    Kind {
        /// The codec's wire tag.
        codec_tag: u8,
        /// The resume cursor, when the kind's contract admits one.
        after_seq: Option<u64>,
    },
    /// A stream whose codec this build does not know. A client refuses
    /// that one resource and ignores its stream; the connection stays.
    Unknown {
        /// The unrecognised [`BootstrapCodec`] tag.
        codec_tag: u8,
    },
}

impl BootstrapStreamProfile {
    /// The codec `BOOTSTRAP_BEGIN` names for this stream.
    #[must_use]
    pub const fn codec(self) -> BootstrapCodec {
        match self {
            Self::NativeState { codec } => BootstrapCodec::Native(codec),
            Self::SynthesizedVtRaw | Self::SynthesizedVtStateSync => BootstrapCodec::SynthesizedVtV1,
            Self::AgentEventsJsonlV1 | Self::AgentEventsJsonlResumeV1 { .. } => {
                BootstrapCodec::AgentEventsJsonlV1
            }
            Self::AccessibilityTreeJsonV1 => BootstrapCodec::AccessibilityTreeJsonV1,
            Self::PixelSurfacePngV1 => BootstrapCodec::PixelSurfacePngV1,
            Self::DocumentTypedV1 => BootstrapCodec::DocumentTypedV1,
            Self::Kind { codec_tag, .. } => BootstrapCodec::Kind { tag: codec_tag },
            Self::Unknown { codec_tag } => BootstrapCodec::Unknown { tag: codec_tag },
        }
    }

    /// The output mode `BOOTSTRAP_BEGIN` names: `StateSync` only for a
    /// synthesized Terminal stream that negotiated it, raw otherwise.
    #[must_use]
    pub const fn output_mode(self) -> OutputMode {
        match self {
            Self::SynthesizedVtStateSync => OutputMode::StateSync,
            _ => OutputMode::Raw,
        }
    }

    /// The record the stream resumes after, if it is a resume.
    #[must_use]
    pub const fn resume_after_seq(self) -> Option<u64> {
        match self {
            Self::AgentEventsJsonlResumeV1 { after_seq } => Some(after_seq),
            Self::Kind { after_seq, .. } => after_seq,
            _ => None,
        }
    }

    /// The catalog row of the kind that streams this profile, or `None` for
    /// a codec this build does not know.
    #[must_use]
    pub const fn kind_spec(self) -> Option<&'static crate::kinds::KindSpec> {
        crate::kinds::codec_kind(self.codec().tag())
    }

    /// The kind that streams this profile, or `None` for a codec this
    /// build does not know.
    #[must_use]
    pub const fn kind(self) -> Option<crate::ids::ResourceKind> {
        match self.kind_spec() {
            Some(spec) => Some(spec.kind),
            None => None,
        }
    }

    /// The stream of a kind-fixed `codec` under `catalog`, resuming after
    /// `after_seq`: `None` when no catalog kind streams it outside the
    /// Terminal profile, or its kind does not resume.
    #[must_use]
    pub const fn for_kind_codec_in(
        catalog: &'static [crate::kinds::KindSpec],
        codec: BootstrapCodec,
        after_seq: Option<u64>,
    ) -> Option<Self> {
        let Some(spec) = crate::kinds::codec_kind_in(catalog, codec.tag()) else {
            return None;
        };
        if spec.stream.needs_terminal_profile || (after_seq.is_some() && spec.stream.resume.is_none())
        {
            return None;
        }
        Some(match (codec, after_seq) {
            (BootstrapCodec::AgentEventsJsonlV1, None) => Self::AgentEventsJsonlV1,
            (BootstrapCodec::AgentEventsJsonlV1, Some(after_seq)) => {
                Self::AgentEventsJsonlResumeV1 { after_seq }
            }
            (BootstrapCodec::AccessibilityTreeJsonV1, None) => Self::AccessibilityTreeJsonV1,
            (BootstrapCodec::PixelSurfacePngV1, None) => Self::PixelSurfacePngV1,
            (BootstrapCodec::DocumentTypedV1, None) => Self::DocumentTypedV1,
            (BootstrapCodec::Kind { tag }, after_seq) => Self::Kind {
                codec_tag: tag,
                after_seq,
            },
            _ => return None,
        })
    }
}

impl BootstrapProfile {
    /// The stream profile a Terminal stream repeats under this connection
    /// profile.
    #[must_use]
    pub const fn terminal_stream(self) -> BootstrapStreamProfile {
        match self {
            Self::NativeState { codec, .. } => BootstrapStreamProfile::NativeState { codec },
            Self::SynthesizedVtRaw => BootstrapStreamProfile::SynthesizedVtRaw,
            Self::SynthesizedVtStateSync => BootstrapStreamProfile::SynthesizedVtStateSync,
        }
    }
}
