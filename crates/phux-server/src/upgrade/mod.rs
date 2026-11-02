//! Graceful server upgrade (ADR-0032): the versioned state blob handed from
//! the old image to the re-exec'd one. Orchestration lives in
//! `runtime::upgrade`.

pub mod blob;

pub(crate) use phux_resource::handoff_env::{HANDOFF_ENV_VARS, SNAPSHOT_DIR_ENV, SOURCE_EXE_ENV};
