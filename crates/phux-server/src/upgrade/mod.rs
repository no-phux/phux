//! Graceful server upgrade (ADR-0032): the versioned state blob handed from
//! the old image to the re-exec'd one. Orchestration lives in
//! `runtime::upgrade`.

pub mod blob;

/// Installed executable the next upgrade pins, handed to the re-exec'd image.
pub(crate) const SOURCE_EXE_ENV: &str = "PHUX_UPGRADE_SOURCE_EXE";
/// The previous image's private executable snapshot, removed on resume.
pub(crate) const SNAPSHOT_DIR_ENV: &str = "PHUX_UPGRADE_SNAPSHOT_DIR";

/// Every environment variable of the upgrade handoff. Server-private: a
/// resumed image removes them once consumed, and pane spawns strip them.
pub(crate) const HANDOFF_ENV_VARS: [&str; 2] = [SOURCE_EXE_ENV, SNAPSHOT_DIR_ENV];
