//! Durable work store and its keeper: the interim, single-host form of the
//! ADR-0092 coordinator.
//!
//! One SQLite file per phux state directory, `work/work-v1.sqlite`, mode
//! `0600`, `PRAGMA user_version = 2`. A newer `user_version` is refused.
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
//! Objective and mail mutations carry an [`OperationId`]. Replaying the same id with
//! the same contents is a no-op; a different body is a conflict. Structural
//! edits bump the objective's revision, and a write that names a stale
//! revision fails.
//!
//! An unresolved `credential_lost` signal projects the objective to
//! `needs_you`. The store will not leave that objective `working`.
//! Agent projection adds `done_unchecked`, `done`, and `failed`. That stamps
//! the store at `user_version` 2 and adds no tables. An older binary refuses
//! the file at open with [`Error::UnsupportedSchema`] instead of failing
//! later on a row it cannot parse. The keeper stays up while any objective
//! is `working` or `queued`.
//!
//! The same keeper holds agent coordination: mail between agents and claims
//! on paths (ADR-0187). Those tables are additive and do not change
//! `user_version`. A harness waits for its agent's mail with
//! [`Client::wait_mail`]: the keeper parks the request until a message
//! arrives or the deadline passes, then hands each message over once.

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
    ADDRESS_ENV, AckMail, Address, Claim, ClaimFilter, ClaimOutcome, DEFAULT_CLAIM_SECS,
    MAX_CLAIM_SECS, MAX_WAIT_MS, Mail, ReadMail, ReleaseClaim, RenewClaim, SendMail, Sent,
    TakeClaim, WaitMail, caller_address, claim_path, pane_address,
};
pub use error::Error;
pub use id::{Alias, ObjectiveState, OpaqueId, OperationId, SignalKind};
pub use service::{Endpoint, KeeperExit, run_keeper, run_keeper_with};
pub use start::{LaunchQueued, OpenRun, StartApplied, StartBinding, StartPlan, StartRequest};
pub use store::{
    AddObjective, Applied, DB_FILE, EditObjective, ListFilter, Objective, RaiseSignal,
    RevisionEdit, Signal, StateEdit, Store, store_path,
};
