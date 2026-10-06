//! The versioned state blob handed from the old server image to the new one
//! across a graceful upgrade (ADR-0032).
//!
//! On `phux upgrade` the running server serializes its whole live
//! session/window/pane tree, the per-pane PTY handoff (child PID + master fd +
//! a replayable VT snapshot), every `AgentSession` bound to a carried pane
//! (its identity and retained record tail), and the monotonic id counters
//! into a [`StateBlob`], passes it to the re-exec'd binary through an inherited
//! descriptor, and the new image rebuilds itself from it.
//!
//! Identity is by **wire** id (`u32`), never core `SlotMap` id, whose
//! generational tags mean nothing in a fresh process; the new image
//! re-interns each entity under its recorded wire id, and [`Counters`] carry
//! the allocators forward.
//!
//! The blob is a compatibility boundary (old writer, newer reader):
//! [`StateBlob::version`] gates incompatible changes, additive fields use
//! `#[serde(default)]`, and unknown fields are ignored.

use std::os::fd::RawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The blob shape version. Bump on any incompatible change; add new fields
/// with `#[serde(default)]` instead when the change is additive.
pub const BLOB_VERSION: u32 = 1;

/// Errors serializing or deserializing a [`StateBlob`].
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// `serde_json` failed to encode the blob.
    #[error("serialize upgrade state blob: {0}")]
    Serialize(serde_json::Error),
    /// `serde_json` failed to decode the blob bytes.
    #[error("deserialize upgrade state blob: {0}")]
    Deserialize(serde_json::Error),
    /// The blob's version is not one this binary knows how to read.
    #[error("unsupported upgrade blob version {found} (this binary speaks {expected})")]
    Version {
        /// The version stamped into the blob.
        found: u32,
        /// The version this binary understands ([`BLOB_VERSION`]).
        expected: u32,
    },
}

/// The complete serialized server state for a graceful upgrade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateBlob {
    /// Blob shape version; see [`BLOB_VERSION`].
    pub version: u32,
    /// Inherited `UnixListener` descriptor, adopted rather than rebound so
    /// the socket path never goes unbound.
    pub listener_fd: RawFd,
    /// Monotonic id allocators carried forward so new ids never collide with
    /// restored ones.
    pub counters: Counters,
    /// Every session, keyed by wire id.
    pub sessions: Vec<SessionBlob>,
    /// Every window, keyed by wire id.
    pub windows: Vec<WindowBlob>,
    /// Every pane, keyed by wire id, with its PTY handoff + snapshot.
    pub panes: Vec<PaneBlob>,
    /// Every `AgentSession` whose parent pane crosses live (ADR-0103), keyed
    /// by wire id in the same space as [`Self::panes`]. Additive: a blob from
    /// an image that predates carried sessions reads as none, and an older
    /// reader ignores the field and drops the sessions as it always did.
    #[serde(default)]
    pub agent_sessions: Vec<AgentSessionBlob>,
}

impl StateBlob {
    /// Serialize to bytes for the handoff descriptor.
    ///
    /// # Errors
    /// [`BlobError::Serialize`] if `serde_json` encoding fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>, BlobError> {
        serde_json::to_vec(self).map_err(BlobError::Serialize)
    }

    /// Deserialize from the handoff descriptor's bytes, rejecting an
    /// unrecognized [`version`](Self::version) with a clean error.
    ///
    /// # Errors
    /// [`BlobError::Deserialize`] on malformed bytes; [`BlobError::Version`]
    /// when the blob's version is not [`BLOB_VERSION`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BlobError> {
        // Probe the version first so a shape mismatch reports cleanly.
        let probe: VersionProbe = parse_unbounded(bytes)?;
        if probe.version != BLOB_VERSION {
            return Err(BlobError::Version {
                found: probe.version,
                expected: BLOB_VERSION,
            });
        }
        parse_unbounded(bytes)
    }
}

/// Parse `bytes` as JSON with `serde_json`'s recursion limit switched off.
///
/// Each split in [`LayoutBlob`] costs two JSON levels, so the default
/// 128-level limit failed to resume a window of 63+ panes and lost every
/// session. The blob only ever comes from this binary's predecessor over an
/// inherited descriptor, never from a peer, so the limit guards nothing.
fn parse_unbounded<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, BlobError> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.disable_recursion_limit();
    let value = T::deserialize(&mut de).map_err(BlobError::Deserialize)?;
    de.end().map_err(BlobError::Deserialize)?;
    Ok(value)
}

#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

/// Monotonic id allocators that must survive the restart so post-upgrade
/// allocations never collide with restored wire ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "the `next_` prefix names each allocator's role; renaming loses meaning"
)]
pub struct Counters {
    /// Next session wire id (`IdBridge`'s allocator).
    pub next_session_wire_id: u32,
    /// Next terminal/pane wire id.
    pub next_terminal_wire_id: u32,
    /// Next window wire id.
    pub next_window_wire_id: u32,
    /// Next session-touch timestamp (resolves `AttachTarget::Last`).
    pub next_touch_timestamp: u64,
    /// The instance token naming the terminal id space (ADR-0109); it must
    /// change exactly when the allocators start over. `None` from an older
    /// image, which keeps the fresh token the new image minted.
    #[serde(default)]
    pub server_instance: Option<[u8; 16]>,
}

/// A session in the blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBlob {
    /// Stable wire id.
    pub wire_id: u32,
    /// User-facing session name.
    pub name: String,
    /// Member windows, in order, by wire id.
    pub window_wire_ids: Vec<u32>,
    /// Active window wire id, if any.
    pub active_window: Option<u32>,
    /// Creation time as nanoseconds since the Unix epoch (best-effort
    /// fidelity; `0` if unknown).
    #[serde(default)]
    pub created_at_unix_nanos: u128,
    /// Last-touched monotonic timestamp, if the session was ever touched.
    #[serde(default)]
    pub last_touched: Option<u64>,
    /// Frozen session-creation directory (cwd-inheritance = session-root).
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// Whether the session survives its last window (ADR-0105). Additive: a
    /// blob from an image that predates keep-empty sessions reads as `false`.
    #[serde(default)]
    pub keep_empty: bool,
}

/// A window in the blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowBlob {
    /// Stable wire id.
    pub wire_id: u32,
    /// Owning session's wire id.
    pub session_wire_id: u32,
    /// Member panes, in insertion order, by wire id.
    pub pane_wire_ids: Vec<u32>,
    /// Active pane wire id, if any.
    pub active_resource: Option<u32>,
    /// The split-tree layout over the panes, if any.
    #[serde(default)]
    pub layout: Option<LayoutBlob>,
    /// Most-recent working directory (cwd-inheritance = last-cwd-per-window).
    #[serde(default)]
    pub last_cwd: Option<PathBuf>,
}

/// A pane in the blob: its metadata plus the PTY handoff and rebuild snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBlob {
    /// Stable wire id.
    pub wire_id: u32,
    /// Owning window's wire id.
    pub window_wire_id: u32,
    /// Grid width in cells.
    pub cols: u16,
    /// Grid height in cells.
    pub rows: u16,
    /// Per-cell pixel size, if any client ever reported pixel metrics.
    #[serde(default)]
    pub cell_px: Option<(u16, u16)>,
    /// Working directory.
    pub cwd: PathBuf,
    /// User-set title, if any: what `GET_STATE` reports as `title`. Never
    /// the program's OSC title, which rides in [`osc_title`](Self::osc_title).
    #[serde(default)]
    pub title: Option<String>,
    /// The program's live OSC 0/2 title, if it set one. The resumed image
    /// replays it into the rebuilt terminal engine (what `GET_SCREEN`
    /// reports), never into [`title`](Self::title).
    #[serde(default)]
    pub osc_title: Option<String>,
    /// The `TERM` the child was spawned with.
    pub term: String,
    /// PID of the child, re-adopted via `waitpid` after the re-exec
    /// (`execve` preserves parentage). `None` for a no-PTY pane.
    #[serde(default)]
    pub child_pid: Option<i32>,
    /// PTY master descriptor (its `FD_CLOEXEC` is cleared before the re-exec).
    /// `None` for a no-PTY pane.
    #[serde(default)]
    pub master_fd: Option<RawFd>,
    /// Replayable viewport snapshot (`ED 2` + cells + cursor/mode epilogue),
    /// the same bytes the synthesizer hands a freshly-attaching client.
    pub vt_replay_bytes: Vec<u8>,
    /// Replayable scrollback history that precedes the viewport, or empty.
    #[serde(default)]
    pub scrollback_bytes: Vec<u8>,
    /// How the process ended, for a pane retained after its exit (ADR-0124);
    /// the resumed image closes it with `SERVER_SHUTDOWN` carrying this exit.
    #[serde(default)]
    pub retained_exit: Option<RetainedExitBlob>,
    /// Seconds a still-running pane asked to be retained after its process
    /// exits (ADR-0124), so the request survives the upgrade.
    #[serde(default)]
    pub retain_secs: Option<u32>,
}

/// An `AgentSession` in the blob (ADR-0103): its identity, derived state,
/// and the record stream a resumed engine continues from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionBlob {
    /// Stable wire id, from the same allocator as pane wire ids, so an
    /// integration's `@N` handle keeps naming the session.
    pub wire_id: u32,
    /// Wire id of the parent pane, which must be in [`StateBlob::panes`].
    pub parent_wire_id: u32,
    /// Harness that produces the session's records, e.g. `claude`.
    pub provider: String,
    /// The provider's own opaque session id, when it supplied one.
    #[serde(default)]
    pub native_id: Option<String>,
    /// The state the stream last derived (`working`, `blocked`, `done`).
    #[serde(default)]
    pub state: Option<String>,
    /// The record counter at the cut; the resumed engine stamps
    /// `base_seq + 1` next.
    pub base_seq: u64,
    /// Records retention had evicted before the cut.
    #[serde(default)]
    pub dropped: u64,
    /// Whether the session already recorded `session_end`.
    #[serde(default)]
    pub ended: bool,
    /// The retained records, oldest first, each a stamped JSONL line. Bounded
    /// by the old image's `defaults.agent-log-bytes` ring; the resumed ring
    /// re-prunes to its own ceiling.
    #[serde(default)]
    pub records: Vec<String>,
}

/// A retained pane's exit record (ADR-0124), as the upgrade blob carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedExitBlob {
    /// `_exit(n)` status, when known.
    #[serde(default)]
    pub exit_status: Option<i32>,
    /// The terminating signal, when known.
    #[serde(default)]
    pub signal: Option<i32>,
    /// When the process exited, Unix milliseconds.
    pub exited_at_ms: u64,
    /// When the old image would have purged it, Unix milliseconds.
    pub retained_until_ms: u64,
}

/// A serializable mirror of [`LayoutNode`](phux_core::window::LayoutNode),
/// with panes referenced by wire id instead of core `ResourceId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutBlob {
    /// A single pane, by wire id.
    Leaf(u32),
    /// An interior split between two children.
    Split {
        /// Split axis.
        dir: SplitDirBlob,
        /// Fraction of the parent given to `left`/`top`, in `(0.0, 1.0)` as
        /// `phux_core` enforces (ADR-0012); the blob validates nothing.
        ratio: f32,
        /// Left (horizontal) / top (vertical) child.
        left: Box<Self>,
        /// Right (horizontal) / bottom (vertical) child.
        right: Box<Self>,
    },
}

/// Serializable mirror of
/// [`SplitDir`](phux_core::window::SplitDir).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDirBlob {
    /// Side-by-side.
    Horizontal,
    /// Stacked.
    Vertical,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StateBlob {
        StateBlob {
            version: BLOB_VERSION,
            listener_fd: 7,
            counters: Counters {
                next_session_wire_id: 3,
                next_terminal_wire_id: 5,
                next_window_wire_id: 4,
                next_touch_timestamp: 42,
                server_instance: Some([7; 16]),
            },
            sessions: vec![SessionBlob {
                wire_id: 1,
                name: "main".to_owned(),
                window_wire_ids: vec![1, 2],
                active_window: Some(1),
                created_at_unix_nanos: 1_700_000_000_000_000_000,
                last_touched: Some(7),
                root: Some(PathBuf::from("/home/u/proj")),
                keep_empty: true,
            }],
            windows: vec![WindowBlob {
                wire_id: 1,
                session_wire_id: 1,
                pane_wire_ids: vec![1, 2],
                active_resource: Some(2),
                layout: Some(LayoutBlob::Split {
                    dir: SplitDirBlob::Horizontal,
                    ratio: 0.5,
                    left: Box::new(LayoutBlob::Leaf(1)),
                    right: Box::new(LayoutBlob::Leaf(2)),
                }),
                last_cwd: Some(PathBuf::from("/home/u/proj/src")),
            }],
            panes: vec![
                PaneBlob {
                    wire_id: 1,
                    window_wire_id: 1,
                    cols: 80,
                    rows: 24,
                    cell_px: Some((9, 18)),
                    cwd: PathBuf::from("/home/u/proj"),
                    title: Some("vim".to_owned()),
                    osc_title: Some("nvim README.md".to_owned()),
                    term: "xterm-256color".to_owned(),
                    child_pid: Some(4321),
                    master_fd: Some(11),
                    vt_replay_bytes: b"\x1b[2J\x1b[Hhello".to_vec(),
                    scrollback_bytes: b"old line\r\n".to_vec(),
                    retained_exit: None,
                    retain_secs: None,
                },
                PaneBlob {
                    wire_id: 2,
                    window_wire_id: 1,
                    cols: 80,
                    rows: 24,
                    cell_px: None,
                    cwd: PathBuf::from("/home/u/proj/src"),
                    title: None,
                    osc_title: None,
                    term: "xterm-256color".to_owned(),
                    child_pid: Some(4322),
                    master_fd: Some(12),
                    vt_replay_bytes: vec![],
                    scrollback_bytes: vec![],
                    retained_exit: None,
                    retain_secs: None,
                },
            ],
            agent_sessions: vec![AgentSessionBlob {
                wire_id: 3,
                parent_wire_id: 2,
                provider: "claude".to_owned(),
                native_id: Some("abc".to_owned()),
                state: Some("working".to_owned()),
                base_seq: 9,
                dropped: 4,
                ended: false,
                records: vec![
                    "{\"seq\":9,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{}}\n".to_owned(),
                ],
            }],
        }
    }

    #[test]
    fn round_trips_through_bytes() {
        let blob = sample();
        let bytes = blob.to_bytes().expect("serialize");
        let back = StateBlob::from_bytes(&bytes).expect("deserialize");
        assert_eq!(blob, back);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut blob = sample();
        blob.version = BLOB_VERSION + 1;
        let bytes = blob.to_bytes().expect("serialize");
        match StateBlob::from_bytes(&bytes) {
            Err(BlobError::Version { found, expected }) => {
                assert_eq!(found, BLOB_VERSION + 1);
                assert_eq!(expected, BLOB_VERSION);
            }
            other => panic!("expected version error, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_tolerates_missing_additive_fields() {
        // A minimal blob omitting every `#[serde(default)]` field still loads,
        // proving older writers stay readable.
        let json = r#"{
            "version": 1,
            "listener_fd": 3,
            "counters": {
                "next_session_wire_id": 1,
                "next_terminal_wire_id": 1,
                "next_window_wire_id": 1,
                "next_touch_timestamp": 1
            },
            "sessions": [{
                "wire_id": 1,
                "name": "s",
                "window_wire_ids": [1],
                "active_window": null
            }],
            "windows": [{
                "wire_id": 1,
                "session_wire_id": 1,
                "pane_wire_ids": [1],
                "active_pane": null
            }],
            "panes": [{
                "wire_id": 1,
                "window_wire_id": 1,
                "cols": 80,
                "rows": 24,
                "cwd": "/tmp",
                "term": "xterm-256color",
                "child_pid": 9,
                "master_fd": 10,
                "vt_replay_bytes": []
            }]
        }"#;
        let blob = StateBlob::from_bytes(json.as_bytes()).expect("tolerant decode");
        assert_eq!(blob.sessions[0].root, None);
        assert_eq!(blob.windows[0].layout, None);
        assert_eq!(blob.panes[0].scrollback_bytes, Vec::<u8>::new());
        assert_eq!(blob.panes[0].cell_px, None);
        assert_eq!(
            blob.counters.server_instance, None,
            "ADR-0109: pre-token image"
        );
        assert!(
            blob.agent_sessions.is_empty(),
            "an image that predates carried agent sessions carries none"
        );
    }

    #[test]
    fn deserialize_ignores_unknown_future_fields() {
        // A newer writer's extra field must not break an older reader.
        let json = r#"{
            "version": 1,
            "listener_fd": 3,
            "counters": {
                "next_session_wire_id": 1,
                "next_terminal_wire_id": 1,
                "next_window_wire_id": 1,
                "next_touch_timestamp": 1
            },
            "sessions": [],
            "windows": [],
            "panes": [],
            "some_future_field": {"nested": [1, 2, 3]}
        }"#;
        let blob = StateBlob::from_bytes(json.as_bytes()).expect("forward-tolerant decode");
        assert!(blob.sessions.is_empty());
    }
}
