//! The server's checkout methods over the user's own `git` and `gh`: the
//! bounded [`checkout`] surface on the checkout a Terminal is in. Hooks,
//! config, credentials and ignore rules behave as they do in the terminal.
//! Calls block; callers run them off their async runtime.

#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-private helpers stay `pub(super)`; `unreachable_pub` rejects plain `pub`"
)]

#[cfg(unix)]
mod bounded;
#[cfg(unix)]
pub mod checkout;
#[cfg(unix)]
mod status;
#[cfg(all(test, unix))]
mod tests;

/// Where a change sits relative to the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Area {
    /// Unmerged: both sides changed it.
    Conflicted,
    /// In the index, ready to commit.
    Staged,
    /// Tracked and changed in the working tree.
    Unstaged,
    /// Not tracked yet.
    Untracked,
}
