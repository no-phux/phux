//! Argument and reply payload types the [`Command`] variants carry.

#[cfg(doc)]
use super::{Command, ErrorCode};

wire_enum! { to_u8 / from_u8;
/// Event class filter for [`Command::SubscribeResourceEvents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceEventType {
    /// Shell state transition (awaiting input → running → idle).
    ShellStateChanged = 0,
    /// Command started (OSC-133 B marker or equivalent).
    CommandStarted = 1,
    /// Command exited with exit code (OSC-133 D marker).
    CommandEnded = 2,
    /// Output arrived on terminal (PTY bytes detected).
    OutputReceived = 3,
    /// Shell prompt ready for input (no output + OSC-133 C or heuristic).
    PromptReady = 4,
    /// Grid mutated (scroll, output, cursor, clear).
    GridChanged = 5,
    /// Working directory changed.
    CwdChanged = 6,
}
}

/// Scope argument for [`Command::GetState`] (SPEC §5.1, ADR-0021).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateScope {
    /// Snapshot the entire server (every Terminal the caller may see).
    Server,
}

wire_enum! { to_u8 / from_u8;
/// Acquisition mode for [`Command::AcquireInput`] (ADR-0033).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputMode {
    /// Grant only if the lease is free; otherwise
    /// [`ErrorCode::InputLeaseHeld`].
    Cooperative = 0,
    /// Preempt the current holder.
    Seize = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// A POSIX signal for a Terminal's process group via
/// [`Command::SignalTerminal`] (ADR-0033). Unlike `KILL_RESOURCE`, the pane
/// stays addressable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalSignal {
    /// SIGINT — the Ctrl-C equivalent; lets the process clean up.
    Interrupt = 0,
    /// SIGSTOP — pause the process group; fully reversible via `Resume`.
    Freeze = 1,
    /// SIGCONT — resume a frozen process group.
    Resume = 2,
    /// SIGTERM — request graceful termination.
    Terminate = 3,
    /// SIGKILL — force termination.
    Kill = 4,
}
}

wire_enum! { to_u8 / from_u8;
/// Lifecycle evidence supplied by an integration hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportedAgentState {
    /// A turn began or resumed.
    Working = 0,
    /// The agent is waiting for human input.
    Blocked = 1,
    /// A turn completed.
    Done = 2,
}
}

/// [`Command::GetScreen::format`]'s low 7 bits: which rendering to
/// produce (D9). `0` none, `1` HTML, `2` VT; `3..=127` are undefined and
/// refused with `INVALID_COMMAND`.
pub const GET_SCREEN_FORMAT_SELECTOR_MASK: u8 = 0x7F;

/// [`Command::GetScreen::format`]'s high bit: join soft-wrapped rows.
/// Ignored when the selector is `0`.
pub const GET_SCREEN_FORMAT_UNWRAP: u8 = 0x80;

/// The transport a [`Command::OpenListener`] asks for (`u8` on the wire).
///
/// Only [`Self::Quic`] is defined. A decoder keeps any other value, so a
/// server can refuse it by name instead of the whole frame failing to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListenerTransport {
    /// QUIC over TLS 1.3, admitting a listener-scoped bearer token (`0x00`).
    Quic,
    /// A value this build does not define. A server refuses it with
    /// `INVALID_COMMAND`.
    Unknown(u8),
}

impl ListenerTransport {
    /// Classify a wire byte, keeping values this build does not define.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Quic,
            other => Self::Unknown(other),
        }
    }

    /// The wire byte.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Quic => 0,
            Self::Unknown(value) => value,
        }
    }
}

/// The operation a [`Command::PerfControl`] asks for (`u8` on the wire).
///
/// A decoder keeps any undefined value, so a server can refuse it by name
/// instead of the whole frame failing to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PerfOp {
    /// Start recording a span trace (`0x00`).
    TraceStart,
    /// Stop the trace and write it to a file (`0x01`).
    TraceStop,
    /// Turn heap accounting on (`0x02`).
    AllocOn,
    /// Turn heap accounting off (`0x03`).
    AllocOff,
    /// A value this build does not define. A server refuses it with
    /// `INVALID_COMMAND`.
    Unknown(u8),
}

impl PerfOp {
    /// Classify a wire byte, keeping values this build does not define.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::TraceStart,
            1 => Self::TraceStop,
            2 => Self::AllocOn,
            3 => Self::AllocOff,
            other => Self::Unknown(other),
        }
    }

    /// The wire byte.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::TraceStart => 0,
            Self::TraceStop => 1,
            Self::AllocOn => 2,
            Self::AllocOff => 3,
            Self::Unknown(value) => value,
        }
    }
}

/// The preconditions a [`Command::KillResourceIf`] carries (ADR-0109).
///
/// Wire body, after the tagged `ResourceId`: an `Option` tag (`0`/`1`), the
/// 16 instance bytes when it is `1`, then the condition bits as one `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KillPrecondition {
    /// When `Some`, the server's current instance token must equal it: the
    /// id is meaningful only in the id space the caller learned it from.
    pub instance: Option<crate::ids::ServerInstance>,
    /// Further conditions, as a bitset.
    pub conditions: KillConditions,
}

impl KillPrecondition {
    /// The precondition for a late kill of a resource this client spawned
    /// and bound to `instance`: the id space must be unchanged, and no other
    /// connection may have attached or used it since.
    #[must_use]
    pub const fn spawned_and_unattached(instance: crate::ids::ServerInstance) -> Self {
        Self {
            instance: Some(instance),
            conditions: KillConditions::UNATTACHED_SINCE_SPAWN,
        }
    }
}

/// Condition bits of a [`KillPrecondition`] (`u8` on the wire).
///
/// A decoder keeps bits it does not know, and a server that receives one
/// refuses the kill with `PRECONDITION_FAILED`: an unknown condition can never
/// be ignored into an unconditional kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KillConditions(u8);

impl KillConditions {
    /// No condition beyond the instance token.
    pub const NONE: Self = Self(0);
    /// No connection other than the one that spawned the resource has
    /// attached or used it since it was spawned, and it has no child
    /// resource. Requires [`KillPrecondition::instance`]; a server refuses the
    /// bit without it (`docs/protocol.md#receipts-retries-and-backpressure` defines it exactly).
    pub const UNATTACHED_SINCE_SPAWN: Self = Self(0x01);
    /// Every bit this build assigns a meaning to.
    const KNOWN: u8 = Self::UNATTACHED_SINCE_SPAWN.0;

    /// Wrap raw wire bits, keeping unknown ones.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw wire bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `true` iff every bit of `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The set with `other`'s bits added.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The bits this build does not know; nonzero means the server must
    /// refuse.
    #[must_use]
    pub const fn unknown_bits(self) -> u8 {
        self.0 & !Self::KNOWN
    }
}

/// Acknowledgement for one [`Command::PutFile`] chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUploadAck {
    /// The next byte offset the server expects.
    pub next_offset: u64,
    /// Absolute completed path, present only after the final digest verifies.
    pub path: Option<String>,
}

/// One [`Command::GetFile`] reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunk {
    /// The file's length when this chunk was read.
    pub size: u64,
    /// Bytes from the requested offset; empty at or past the end.
    pub data: Vec<u8>,
    /// Whole-file SHA-256, present only on the chunk that reaches the end.
    pub sha256: Option<[u8; 32]>,
}
