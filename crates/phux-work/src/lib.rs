//! Durable work store and its keeper: the interim, single-host form of the
//! ADR-0092 coordinator.
//!
//! One SQLite file per phux state directory, `work/work-v1.sqlite`, mode
//! `0600`, `PRAGMA user_version = 1`. An unknown `user_version` is refused.
//! Rows use the coordinator nouns Objective, Run, binding, Signal, and
//! event, so the coordinator can replay them. Primary keys are opaque
//! 16-byte ids. A short alias such as `phux/3` is a display name stored
//! beside the id and allocated atomically per repo.
//!
//! One keeper process per store holds the store lock and is its only
//! writer ([`run_keeper`]). Every surface calls it through [`Client`], which
//! starts a keeper when none answers. Nothing outside the keeper opens a
//! [`Store`].
//!
//! Every mutation carries an [`OperationId`]. Replaying the same id with
//! the same contents is a no-op; a different body is a conflict. Structural
//! edits bump the objective's revision, and a write that names a stale
//! revision fails.
//!
//! An unresolved `credential_lost` signal projects the objective to
//! `needs_you`. The store will not leave that objective `working`.
//!
//! The same keeper holds agent coordination: mail between agents and claims
//! on paths (ADR-0187). Those tables are additive and do not change
//! `user_version`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Private modules share helpers. `pub` would trip `unreachable_pub`;
// `pub(crate)` is the visibility those helpers actually have.
#![allow(clippy::redundant_pub_crate)]

mod client;
mod coord;
mod error;
mod id;
mod lock;
mod schema;
mod service;
mod start;
mod store;
mod wire;

pub use client::{Client, Launcher};
pub use coord::{
    AckMail, Address, Claim, ClaimFilter, ClaimOutcome, MAX_CLAIM_SECS, Mail, ReadMail,
    ReleaseClaim, SendMail, Sent, TakeClaim,
};
pub use error::Error;
pub use id::{Alias, ObjectiveState, OpaqueId, OperationId, SignalKind};
pub use service::{Endpoint, KeeperExit, run_keeper};
pub use start::{StartApplied, StartBinding, StartPlan, StartRequest};
pub use store::{
    AddObjective, Applied, DB_FILE, EditObjective, ListFilter, Objective, RaiseSignal,
    RevisionEdit, Signal, StateEdit, Store, store_path,
};
