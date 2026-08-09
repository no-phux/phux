//! The versioned state blob handed from the old server image to the new one
//! across a graceful upgrade (ADR-0032).
//!
//! On `phux upgrade` the running server serializes its whole live
//! session/window/pane tree, the per-pane PTY handoff (child PID + master fd +
//! a replayable VT snapshot), and the monotonic id counters into a
//! [`StateBlob`], passes it to the re-exec'd binary through an inherited
//! descriptor, and the new image rebuilds itself from it.
//!
//! # Identity is by *wire* id, never core id
//!
//! The in-memory [`Registry`](phux_core::registry::Registry) keys everything
//! by `SlotMap` ids whose generational tags are meaningless in a fresh
//! process. The blob is therefore keyed entirely by the **wire** ids (`u32`)
//! the server already mints and hands clients — the new image re-interns each
//! entity under its recorded wire id, which both preserves client-visible
//! identity and rebuilds the wire↔core maps. The `*_counter` fields carry the
//! allocators forward so freshly-created entities never collide with restored
//! ones.
//!
//! # Compatibility
//!
//! The blob is a maintained compatibility boundary: the *old* binary writes it
//! and a *newer* binary reads it. [`StateBlob::version`] gates incompatible
//! shape changes; within a version, additive fields use `#[serde(default)]` so
//! a newer reader tolerates an older writer, and `serde_json`'s
//! ignore-unknown-fields behaviour lets an older reader tolerate a newer
//! writer's extra fields. The JSON carrier is self-describing and zero new
//! deps; binary fields ride as number arrays for now (TODO: a compact carrier
//! such as `postcard` if snapshot size becomes a concern).
//!
//! # Depth
//!
//! [`LayoutBlob`] is an externally-tagged enum, so every split costs **two**
//! JSON object levels (`{"Split":{...}}`), and
//! [`Registry::new_terminal`](phux_core::registry::Registry::new_terminal)
//! nests one split per pane down a single spine. Against `serde_json`'s
//! default 128-level recursion limit that put the ceiling at **62 panes in one
//! window**: 62 round-tripped, 63 serialized fine and then failed to parse,
//! surfacing as `ServerError::Resume` and losing the entire server state —
//! every session, not just the offending window. [`StateBlob::from_bytes`]
//! therefore parses with the recursion limit disabled; the justification for
//! that trade is on `parse_unbounded`. Raising the ceiling does not remove
//! it: `phux_core` still imposes no depth bound of its own, while
//! `phux_protocol`'s wire codec caps at `MAX_LAYOUT_DEPTH` (64), so a window
//! deeper than that is un-sendable regardless. The cross-encoding map lives in
//! `crates/phux/tests/layout_conformance.rs`.

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
    /// Inherited `UnixListener` descriptor (its `FD_CLOEXEC` is cleared before
    /// the re-exec so it survives). The new image rebinds nothing — it adopts
    /// this fd, keeping the socket path bound with no rebind race.
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
        // Read just the version first so a shape mismatch reports as a clean
        // version error rather than a deep serde decode failure. Note this
        // still walks the whole document, so it needs the same depth
        // treatment as the full parse below.
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
/// `serde_json::from_slice` caps nesting at 128 levels, which is the right
/// default for JSON off a network but the wrong one here — see the `# Depth`
/// section of this module's docs. The blob is written by *this binary's own
/// predecessor image* into an anonymous temp file and handed over on an
/// inherited descriptor across `execve`
/// (`crates/phux-server/src/runtime/upgrade.rs`); it never comes from a peer,
/// a client, or the filesystem at large. The limit therefore guards nothing
/// reachable, while costing the loss of every session on the server the first
/// time a user splits one window past 62 panes. Depth here is bounded in
/// practice by the pane count of a single window, which the human has to
/// create one pane at a time.
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
    pub active_pane: Option<u32>,
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
    /// User-set title, if any.
    #[serde(default)]
    pub title: Option<String>,
    /// The `TERM` the child was spawned with.
    pub term: String,
    /// PID of the child on the slave side — re-adopted via `waitpid` after the
    /// re-exec (sound because `execve` preserves process parentage). `None`
    /// for a no-PTY pane, which carries no child to hand off.
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
}

/// A serializable mirror of [`LayoutNode`](phux_core::window::LayoutNode),
/// with panes referenced by wire id instead of core `TerminalId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutBlob {
    /// A single pane, by wire id.
    Leaf(u32),
    /// An interior split between two children.
    Split {
        /// Split axis.
        dir: SplitDirBlob,
        /// Fraction of the parent given to `left`/`top`.
        ///
        /// The blob is a core→core carrier and validates nothing: it inherits
        /// whatever domain `phux_core` enforced on the way in, which per
        /// ADR-0012 is the *open* interval `(0.0, 1.0)`. A non-finite value
        /// would serialize as JSON `null` and then fail to parse, taking the
        /// whole blob with it — nothing can produce one today because
        /// `Window::split` rejects NaN at the constructor. The three
        /// encodings' ratio domains are mapped in
        /// `crates/phux/tests/layout_conformance.rs`.
        ratio: f32,
        /// Left (horizontal) / top (vertical) child.
        left: Box<LayoutBlob>,
        /// Right (horizontal) / bottom (vertical) child.
        right: Box<LayoutBlob>,
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
            },
            sessions: vec![SessionBlob {
                wire_id: 1,
                name: "main".to_owned(),
                window_wire_ids: vec![1, 2],
                active_window: Some(1),
                created_at_unix_nanos: 1_700_000_000_000_000_000,
                last_touched: Some(7),
                root: Some(PathBuf::from("/home/u/proj")),
            }],
            windows: vec![WindowBlob {
                wire_id: 1,
                session_wire_id: 1,
                pane_wire_ids: vec![1, 2],
                active_pane: Some(2),
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
                    term: "xterm-256color".to_owned(),
                    child_pid: Some(4321),
                    master_fd: Some(11),
                    vt_replay_bytes: b"\x1b[2J\x1b[Hhello".to_vec(),
                    scrollback_bytes: b"old line\r\n".to_vec(),
                },
                PaneBlob {
                    wire_id: 2,
                    window_wire_id: 1,
                    cols: 80,
                    rows: 24,
                    cell_px: None,
                    cwd: PathBuf::from("/home/u/proj/src"),
                    title: None,
                    term: "xterm-256color".to_owned(),
                    child_pid: Some(4322),
                    master_fd: Some(12),
                    vt_replay_bytes: vec![],
                    scrollback_bytes: vec![],
                },
            ],
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
