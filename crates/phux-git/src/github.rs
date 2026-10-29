//! Pull requests on GitHub through the user's own `gh`.
//!
//! The review inbox, one pull request's files, threads and checks, the writes
//! a reviewer makes, a failed job's log, and the pull request's patch from a
//! local clone.
//!
//! Every call blocks on the network; callers run them off their UI thread.
//!
//! | Module | Owns |
//! |---|---|
//! | [`gh`] | the bounded `gh` run and GraphQL transport |
//! | [`inbox`] | open pull requests that want the viewer |
//! | [`pull`] | one pull request in full |
//! | [`mod@write`] | reviews, replies, resolution, viewed marks, merges |
//! | [`logs`] | a failed job's log, cut to what failed |
//! | [`patch`] | the pull request's patch, locally or from GitHub |

pub mod gh;
pub mod inbox;
pub mod logs;
pub mod patch;
mod process;
pub mod pull;
pub mod types;
pub mod write;

pub use gh::GhError;
pub use inbox::{Inbox, InboxPull, Reasons};
pub use pull::{CiCheck, Entry, Note, Pull, PullFile, Rights, Thread};
pub use types::{
    CheckState, LineSide, MergeMethod, MergeMethods, MergeState, Mergeability, PrState,
    ReviewDecision, ViewedState,
};
pub use write::{NewThread, ReviewEvent};

/// `owner/name` split in two, or `None` when it is not that shape.
#[must_use]
pub fn split_repo(repo: &str) -> Option<(&str, &str)> {
    let (owner, name) = repo.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then_some((owner, name))
}
