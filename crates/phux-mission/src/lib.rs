//! Local mission task store.
//!
//! One SQLite file per phux state directory, `mission/mission-v1.sqlite`,
//! mode `0600`, `PRAGMA user_version = 1`. An unknown `user_version` is
//! refused. Rows use the coordinator nouns Objective, Run, binding, Signal,
//! and event, so a later coordinator can replay them. Primary keys are
//! opaque 16-byte ids. A short alias such as `phux/3` is a display name
//! stored beside the id and allocated atomically per repo.
//!
//! Every mutation carries an [`OperationId`]. Replaying the same id with
//! the same contents is a no-op; a different body is a conflict. Structural
//! edits bump the objective's revision, and a write that names a stale
//! revision fails. A [`Store`] holds an exclusive file lock for its life,
//! and each mutation is one transaction.
//!
//! An unresolved `credential_lost` signal projects the objective to
//! `needs_you`. The store will not leave that objective `working`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Private modules share helpers. `pub` would trip `unreachable_pub`;
// `pub(crate)` is the visibility those helpers actually have.
#![allow(clippy::redundant_pub_crate)]

mod error;
mod id;
mod json;
mod lock;
mod schema;
mod store;

pub use error::Error;
pub use id::{Alias, ObjectiveState, OpaqueId, OperationId, SignalKind};
pub use json::{
    ListDocument, MutationDocument, SCHEMA, SCHEMA_VERSION, ShowDocument, TaskDocument, Verb,
};
pub use store::{
    AddObjective, Applied, DB_FILE, EditObjective, ListFilter, Objective, RaiseSignal,
    RevisionEdit, Signal, StateEdit, Store, store_path,
};
