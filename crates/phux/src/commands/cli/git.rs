//! `phux git`: typed reads of the checkout a pane is in, over `CHECKOUT`.

use usage::{Args, Subcommands};

use super::JsonOpt;

/// Read the Git checkout a pane is in
///
/// The server that owns the pane resolves the checkout from the pane's live
/// working directory and runs Git there, so a pane on a satellite reads that
/// host's checkout. Nothing attaches. A typed refusal (`not_a_repository`,
/// `git_unavailable`, `no_pull_request`, ...) exits 1 with the answer still
/// printed under `--json`.
#[derive(Debug, Args)]
pub(crate) struct GitArgs {
    #[usage(subcommand)]
    pub(crate) action: GitAction,
}

#[derive(Debug, Subcommands)]
pub(crate) enum GitAction {
    /// Branch, upstream, and every changed file with line counts.
    Status {
        /// Target selector: session, session:group, session:group.pane,
        /// @id, or `.` (focused).
        target: String,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// One file's unified diff (unstaged unless --staged or --untracked).
    Diff {
        /// Target selector, as for `status`.
        target: String,
        /// Checkout-relative path.
        path: String,
        /// The index against `HEAD`.
        #[usage(long, conflicts("--untracked"))]
        staged: bool,
        /// An untracked file, as a whole new file.
        #[usage(long)]
        untracked: bool,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// One page of history, newest first.
    Log {
        /// Target selector, as for `status`.
        target: String,
        /// Only commits touching this checkout-relative path.
        path: Option<String>,
        /// Commits to skip from `HEAD`.
        #[usage(long, default = "0", value_name = "N")]
        skip: u32,
        /// Commits to return, 1 to 100.
        #[usage(long, default = "20", value_name = "N")]
        limit: u32,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// Local and remote-tracking branches.
    Branches {
        /// Target selector, as for `status`.
        target: String,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// The current branch's pull request and its checks, from the host's `gh`.
    Pr {
        /// Target selector, as for `status`.
        target: String,
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// One commit: message, files with line counts, and the patch.
    Show {
        /// Target selector, as for `status`.
        target: String,
        /// A commit id or a prefix of one (4 to 64 hex digits).
        sha: String,
        #[usage(flatten)]
        json: JsonOpt,
    },
}
