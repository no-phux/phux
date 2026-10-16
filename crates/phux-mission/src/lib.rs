//! Mission control: the task vocabulary people and agents see, composed over
//! the work store in `phux-work`.
//!
//! This crate owns the `phux.mission/v1` documents. It never opens the
//! store; it renders what a [`phux_work::Client`] returns. The board, Lead,
//! and `start` land here.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod json;

pub use json::{
    ListDocument, MutationDocument, SCHEMA, SCHEMA_VERSION, ShowDocument, TaskDocument, Verb,
};
