//! Graceful server upgrade (ADR-0032).
//!
//! Re-exec the binary in place while the PTYs, their children, and the
//! listening socket survive on inherited descriptors, then rebuild the session
//! tree from a replayed VT snapshot.
//!
//! This module is built in slices:
//! - [`blob`] — the versioned state blob handed old image → new image.
//!
//! Subsequent slices add the producer (reading live [`ServerState`] +
//! per-pane handles into a [`blob::StateBlob`]), the `--resume` consumer that
//! adopts the inherited descriptors via `portable-pty-adopt`, the re-exec
//! orchestration with its never-strand-a-child fallback, and the
//! `phux upgrade` control command.
//!
//! [`ServerState`]: crate::state::ServerState

pub mod blob;

/// Installed executable the next upgrade pins, handed to the re-exec'd image.
pub(crate) const SOURCE_EXE_ENV: &str = "PHUX_UPGRADE_SOURCE_EXE";
/// The previous image's private executable snapshot, removed on resume.
pub(crate) const SNAPSHOT_DIR_ENV: &str = "PHUX_UPGRADE_SNAPSHOT_DIR";

/// Every environment variable of the old-image to new-image handoff. They
/// are server-private (phux-m5yj): a resumed image removes them from its own
/// environment once consumed, and pane spawns strip them from the child.
pub(crate) const HANDOFF_ENV_VARS: [&str; 2] = [SOURCE_EXE_ENV, SNAPSHOT_DIR_ENV];
