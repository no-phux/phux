//! The upgrade handoff's environment. Server-private: a resumed image
//! removes these once consumed, and every process an app spawns strips them.

/// Installed executable the next upgrade pins, handed to the re-exec'd image.
pub const SOURCE_EXE_ENV: &str = "PHUX_UPGRADE_SOURCE_EXE";
/// The previous image's private executable snapshot, removed on resume.
pub const SNAPSHOT_DIR_ENV: &str = "PHUX_UPGRADE_SNAPSHOT_DIR";

/// Every environment variable of the upgrade handoff.
pub const HANDOFF_ENV_VARS: [&str; 2] = [SOURCE_EXE_ENV, SNAPSHOT_DIR_ENV];
