wire_enum! { as_wire / from_wire;
/// A client's color tier, most to least permissive. The server
/// rewrites outbound VT bytes to fit; `TrueColor` is the default so an
/// unadvertised client is never silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorSupport {
    /// 24-bit direct RGB; SGR truecolor is forwarded verbatim.
    #[default]
    TrueColor = 0,
    /// xterm 256-color palette.
    Indexed256 = 1,
    /// 16 system colors only.
    Indexed16 = 2,
    /// Monochrome: SGR color sequences MUST be stripped.
    Mono = 3,
}
}

wire_enum! { as_wire / from_wire else Raw;
/// How the server emits terminal content to this consumer.
///
/// Either the byte-faithful raw PTY broadcast, or a per-consumer synthesized
/// state-sync tick for agent / remote consumers that want a coherent grid
/// model. An unknown wire tag decodes as [`OutputMode::Raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OutputMode {
    /// Raw PTY byte broadcast: the default, human-TUI path.
    #[default]
    Raw = 0,
    /// Per-consumer synthesized grid deltas with a monotonic `seq` (ADR-0018).
    StateSync = 1,
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

wire_enum! { as_u8 / from_u8 else None;
/// A frame-payload compression algorithm (`docs/protocol.md#framing`).
///
/// Decode-invisible: `FRAME_COMPRESSED` inflates back to the exact inner
/// frame bytes, so native records stay byte-identical end to end.
/// An unknown algorithm reads as `None`, so an out-of-contract peer fails on
/// its first wrapped frame instead of being misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Compression {
    /// No transform. Frames go on the wire exactly as encoded.
    #[default]
    None = 0,
    /// Raw DEFLATE (RFC 1951), as produced by `flate2`'s `miniz_oxide` backend.
    Deflate = 1,
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
