//! Shared sub-record payload types: attach targets (SPEC §13), viewport
//! info, L3 metadata scope (SPEC §7.4), and spawn/move results (SPEC §10.1).

use crate::ids::{GroupId, SessionId, TerminalId};

// -----------------------------------------------------------------------------
// AttachTarget tagged union — SPEC §13.
// -----------------------------------------------------------------------------

/// Session the client wishes to attach to, per SPEC §13.
///
/// Tagged union; each variant maps to one of SPEC's four selection modes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttachTarget {
    /// Most-recently-touched live session known to the server. Before any
    /// touch, resolves to the server's configured live seed. Returns
    /// `SESSION_NOT_FOUND` when neither resolution yields a live session;
    /// never creates.
    Last,
    /// Look up a session by its human-readable name.
    ByName(String),
    /// Look up a session by its server-assigned [`SessionId`].
    ById(SessionId),
    /// Look up a session by name; create one if no such session exists.
    CreateIfMissing {
        /// Name for the new session (also used to match an existing one).
        name: String,
        /// Initial command to run in the seed pane, if creation occurs.
        command: Option<Vec<String>>,
        /// Working directory for the seed pane, if creation occurs.
        cwd: Option<String>,
    },
}

/// Viewport metrics the client advertises at attach time.
///
/// SPEC §13: `{ cols, rows, pixel_w: optional<u16>, pixel_h: optional<u16> }`.
/// Pixel dimensions support sub-cell rendering and image protocols; cells are
/// the load-bearing axis.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_pixels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ViewportInfo {
    /// Viewport width in cells.
    pub cols: u16,
    /// Viewport height in cells.
    pub rows: u16,
    /// Optional viewport width in pixels.
    pub pixel_w: Option<u16>,
    /// Optional viewport height in pixels.
    pub pixel_h: Option<u16>,
}

impl ViewportInfo {
    /// Construct a `ViewportInfo` from cell dimensions, the load-bearing
    /// axis per SPEC §13. Pixel dimensions default to `None`; supply them
    /// via [`Self::with_pixels`] when the host kernel reports them.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_w: None,
            pixel_h: None,
        }
    }

    /// Builder setter for the optional pixel dimensions (`pixel_w`,
    /// `pixel_h`). Pass `None` for either axis the kernel did not report.
    #[must_use]
    pub const fn with_pixels(mut self, pixel_w: Option<u16>, pixel_h: Option<u16>) -> Self {
        self.pixel_w = pixel_w;
        self.pixel_h = pixel_h;
        self
    }
}

// -----------------------------------------------------------------------------
// Scope — SPEC §7.4 / §11.L3 (phux-4li.2). The "where does this key live?"
// tagged union shared by every L3 metadata frame.
// -----------------------------------------------------------------------------

/// Scope of an L3 metadata key (SPEC §7.4 / §11.L3).
///
/// Tagged union:
/// - `Terminal { terminal_id }` — keys scoped to a single Terminal. Killed
///   with the Terminal.
/// - `Group { group_id }` — keys scoped to a Group (opaque grouping key).
///   v0.1 servers expose a single default Group that satisfies the
///   reference TUI's `phux.tui.layout/v1` use case (see ADR-0019).
/// - `Global` — keys scoped to the server (e.g. cross-Group prefs).
///
/// Wire encoding: 1-byte tag + per-variant body.
/// - tag `0x00` → `Terminal`, body = tagged `TerminalId`.
/// - tag `0x01` → `Group`, body = `u32` (the inner `GroupId`).
/// - tag `0x02` → `Global`, body = empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scope {
    /// Keys scoped to a single Terminal. Cleared when the Terminal closes.
    Terminal(TerminalId),
    /// Keys scoped to a Group (opaque grouping key).
    Group(GroupId),
    /// Server-wide keys.
    Global,
}

// -----------------------------------------------------------------------------
// SpawnError / SpawnResult — SPEC §7.2 / §10.1 (phux-4li.10).
//
// `SpawnResult` is the `Result<TerminalId, SpawnError>` carried inside
// `TERMINAL_SPAWNED`. Modelled as a dedicated tagged union (rather than
// reusing the Rust `Result` type directly on the wire) so the codec
// stays in lockstep with the SPEC text and so future error variants can
// land without touching call sites that match on the type.
//
// Both `SpawnResult` and `SpawnError` are `#[non_exhaustive]`: forward-
// compatible additions are protocol-minor changes, mirroring the
// existing [`ErrorCode`] / [`AttachTarget`] / [`Scope`] precedent.
//
// Wire encoding:
//   SpawnResult tag 0x00 Ok  → tagged TerminalId
//   SpawnResult tag 0x01 Err → SpawnError
//   SpawnError  tag 0x00 GroupNotFound → no body
//   SpawnError  tag 0x01 SpawnFailed        → length-prefixed UTF-8 str
// -----------------------------------------------------------------------------

/// Error variants for [`FrameKind::TerminalSpawned`](super::FrameKind::TerminalSpawned), SPEC §7.2 / §10.1.
///
/// `#[non_exhaustive]` so a v0.2.x server may add codes (e.g.
/// `PermissionDenied`, `ResourceExhausted`) without breaking downstream
/// matches. Unknown wire tags surface as
/// [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue) rather than coercing to a
/// placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnError {
    /// The `group` named in [`FrameKind::SpawnTerminal`](super::FrameKind::SpawnTerminal) does not
    /// exist on this server. v0.1 servers expose a single default
    /// Group at `GroupId(1)` (SPEC §7.4 L2-dependency note);
    /// any other id MAY surface this error.
    GroupNotFound,
    /// Spawning the underlying PTY failed for an implementation-specific
    /// reason. The carried string is a human-readable diagnostic — short
    /// enough to log inline; the SPEC does not constrain its contents
    /// beyond UTF-8.
    SpawnFailed(String),
    /// The spawn named a satellite (`SPAWN_TERMINAL.satellite`) but this
    /// server cannot route to it: it is not a federation hub, or the host
    /// is absent from its satellite registry (phux-v45.6). The spawn-reply
    /// mirror of `ErrorCode::UnsupportedSatelliteRoute` — a configuration
    /// refusal, fixed by `phux server --hub` / `phux host add --role satellite`.
    UnsupportedSatelliteRoute,
    /// The spawn named a satellite this hub dials, but the link is down,
    /// dialing, refused fail-closed, or did not answer within the relay
    /// deadline (phux-v45.6). The spawn-reply mirror of
    /// `ErrorCode::SatelliteUnreachable`; carries the same human-readable
    /// diagnostic. Retryable — the hub redials with backoff.
    SatelliteUnreachable(String),
}

/// Tagged union carried by [`FrameKind::TerminalSpawned`](super::FrameKind::TerminalSpawned), SPEC §7.2 / §10.1.
///
/// Either the server-allocated [`TerminalId`] of the freshly spawned
/// Terminal, or a structured [`SpawnError`]. Modelled as a dedicated
/// enum rather than the Rust `core::result::Result` directly so the
/// codec mirrors the SPEC's tagged-union vocabulary and so the
/// `#[non_exhaustive]` contract carries through unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnResult {
    /// The freshly spawned Terminal's identifier.
    Ok(TerminalId),
    /// Structured failure; see [`SpawnError`].
    Err(SpawnError),
}

/// Error variants for [`FrameKind::TerminalMoved`](super::FrameKind::TerminalMoved) (ADR-0056).
///
/// `#[non_exhaustive]` on the same contract as [`SpawnError`]: additive
/// variants are protocol-minor changes, and an unknown wire tag surfaces
/// as [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MoveError {
    /// The re-parent was refused or failed: either Terminal does not
    /// exist, the destination window is gone, or the registry rejected
    /// the move. The carried string is a human-readable diagnostic,
    /// unconstrained beyond UTF-8 (the [`SpawnError::SpawnFailed`]
    /// shape).
    MoveFailed(String),
    /// The move named a satellite-tagged Terminal on either end. A move
    /// is local-only (ADR-0056): federation routing for it does not
    /// exist, matching the spawn-reply mirror
    /// [`SpawnError::UnsupportedSatelliteRoute`].
    UnsupportedSatelliteRoute,
}

/// Tagged union carried by [`FrameKind::TerminalMoved`](super::FrameKind::TerminalMoved) (ADR-0056).
///
/// Either the moved Terminal's (unchanged) [`TerminalId`] — echoed back
/// so a caller can correlate without holding request state — or a
/// structured [`MoveError`]. A move never changes identity: the id is
/// stable across it, so subscriptions and outstanding waits survive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MoveResult {
    /// The moved Terminal's identifier (stable across the move).
    Ok(TerminalId),
    /// Structured failure; see [`MoveError`].
    Err(MoveError),
}
