//! What a [`Command::Mirror`](super::Command::Mirror) asks of a host's
//! screen mirrors (`docs/resource-kinds.md#standing-mirrors`).

/// The longest `target` a [`MirrorOp`] carries, in bytes.
pub const MAX_MIRROR_TARGET: usize = 1024;
/// The longest target list `MIRROR_PROVIDE` carries, in bytes.
pub const MAX_MIRROR_TARGETS: usize = 256 * 1024;

/// One request about a host's screen mirrors. Every reply is
/// `OkWith(Json)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorOp {
    /// List the displays and windows the host can mirror:
    /// `{"targets": [{"target", "kind": "display"|"window", "label"}]}`.
    Targets,
    /// Start the mirror of `target` (as `phux agent surface mirror` takes
    /// it: `display`, `display:OUTPUT`, `App`, `App:title`) in its
    /// `mirror-<target>` session, or find the one already running:
    /// `{"session", "surface"}`, `surface` the local id of its surface, or
    /// null when none was published within the server's wait.
    Start {
        /// What to mirror.
        target: String,
        /// JPEG frames instead of PNG.
        jpeg: bool,
    },
    /// Stop a mirror [`Self::Start`] began: `{"session"}`. A `[[mirrors]]`
    /// one is refused.
    Stop {
        /// The target it was started with.
        target: String,
    },
}
