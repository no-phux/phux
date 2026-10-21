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

    /// A peer's advertisement made valid: a value above the hard cap is the
    /// cap, and `0` (no usable bound) is the default. `HELLO` decodes the
    /// client's limits this way, so a newer client with larger buffers is
    /// clamped, never refused.
    #[must_use]
    pub const fn clamped(max_chunk_bytes: u32, max_history_page_bytes: u32) -> Self {
        const fn clamp(value: u32, cap: u32, default: u32) -> u32 {
            if value == 0 {
                default
            } else if value > cap {
                cap
            } else {
                value
            }
        }
        Self {
            max_chunk_bytes: clamp(
                max_chunk_bytes,
                MAX_BOOTSTRAP_CHUNK_BYTES,
                DEFAULT_BOOTSTRAP_CHUNK_BYTES,
            ),
            max_history_page_bytes: clamp(
                max_history_page_bytes,
                MAX_HISTORY_PAGE_BYTES,
                DEFAULT_HISTORY_PAGE_BYTES,
            ),
        }
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
