//! `phux project` and `phux worktree`.

use crate::commands::RemoteOpt;
use usage::{Args, Subcommands};

/// Open a project session from a trusted recipe
///
/// A project is a checkout. `open` attaches to its session, building it
/// first from the checkout's `.phux/project.toml` recipe (windows,
/// panes, splits, commands, environment, focus) when it is not running.
/// A recipe runs only after its exact bytes are approved on this machine
/// (`trust`); a live session is reopened as it is and its recipe is never
/// rerun. `[[projects]]` entries in config.toml name checkouts so `open
/// NAME` works from anywhere. The session name is the one `phux worktree`
/// derives from the checkout path.
#[derive(Debug, Args)]
pub(crate) struct ProjectArgs {
    #[usage(subcommand)]
    pub(crate) action: ProjectAction,
}

/// Manage git worktrees and their bound sessions
///
/// Each worktree binds to one session whose name is derived from the
/// worktree's directory basename. The derivation is a pure function of
/// the path, so the binding is computed on demand and can never go
/// stale — phux stores no worktree state and the server knows no git.
#[derive(Debug, Args)]
pub(crate) struct WorktreeArgs {
    #[usage(subcommand)]
    pub(crate) action: WorktreeAction,
}

/// `phux project <action>` — named checkouts and trusted recipes.
#[derive(Debug, Subcommands)]
pub(crate) enum ProjectAction {
    /// Attach to a project's session, building it from its recipe if absent.
    ///
    /// TARGET is a `[[projects]]` name or a directory (default: the current
    /// one); inside git, the checkout root is used. An unapproved recipe is
    /// shown and must be approved at the terminal; without a terminal, or
    /// with `--json`, it is refused (exit 2) until `phux project trust`.
    /// No recipe opens one shell at the root.
    Open {
        /// Project name or directory.
        target: Option<String>,

        /// Build the session without attaching and print a stable JSON
        /// document: project, path, session, whether it was created now,
        /// and the seed pane's `terminal_id`.
        #[usage(long)]
        json: bool,

        /// Use declared native SDK integrations when the trusted recipe names
        /// them explicitly; raw terminal commands remain terminal commands.
        #[usage(long)]
        prefer_native: bool,
    },

    /// List the `[[projects]]` catalog and whether each session is open.
    #[usage(alias = "ls")]
    List {
        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Write a starter `.phux/project.toml` into a checkout.
    ///
    /// Refuses to overwrite an existing recipe. The starter is approved as
    /// written; editing it makes it untrusted until `phux project trust`.
    Init {
        /// Project name or directory (default: the current one).
        target: Option<String>,
    },

    /// Validate a recipe and approve its exact bytes on this machine.
    ///
    /// The approval covers every worktree of the same repository and ends
    /// the moment one byte of the recipe changes.
    Trust {
        /// Project name or directory (default: the current one).
        target: Option<String>,
    },

    /// Withdraw this machine's approval of a recipe.
    Untrust {
        /// Project name or directory (default: the current one).
        target: Option<String>,
    },

    /// Report whether a recipe is approved: exit 0 trusted, 1 untrusted or
    /// absent, 2 undeterminable.
    Status {
        /// Project name or directory (default: the current one).
        target: Option<String>,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Link a session into a named project, on any machine.
    ///
    /// The link is stored on the server that owns SESSION, so every client
    /// (desktop, TUI, `phux ls`) sees it, and groups sessions by project
    /// name across hosts: a link outranks the git repository a session was
    /// created in, and a link named like a repository joins that
    /// repository's group. Relinking replaces the link; the last writer
    /// wins. NAME is trimmed.
    Link {
        /// Session name or key (`^ab23cd`).
        session: String,

        /// Project name (at most 256 bytes, no control characters).
        name: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Clear a session's project link; it groups by its repository again.
    ///
    /// Only the link is cleared, never the free-text project a recipe or
    /// archive recorded.
    Unlink {
        /// Session name or key (`^ab23cd`).
        session: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Map a git repository and its checked-out worktrees.
    ///
    /// A local repo operation: it never contacts a phux server and never
    /// creates or deletes a worktree. Agents use it to map checkouts to
    /// sessions before spawning or attaching work.
    Inspect {
        /// Path inside the repository or worktree to inspect.
        #[usage(default = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },
}

/// `phux worktree <action>` — git worktrees bound to sessions by name.
#[derive(Debug, Subcommands)]
pub(crate) enum WorktreeAction {
    /// List the repository's worktrees and their bound sessions.
    ///
    /// The `bound` column reads `live` when a session by the derived name
    /// exists, `-` when it does not, and `?` when no server is running —
    /// "no server" and "no session" are different facts.
    #[usage(alias = "ls")]
    List {
        /// Path inside the repository or worktree to list from.
        #[usage(default = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Create a worktree and a session rooted in it.
    ///
    /// An existing local branch is checked out; a missing one is created,
    /// from `--from` when given and from the current HEAD otherwise. The
    /// worktree lands beside the repository as `<repo>-<branch>` unless
    /// `--path` says otherwise.
    New {
        /// Branch to check out, or to create when it does not exist.
        branch: String,

        /// Where to put the worktree. Defaults to a sibling of the repo.
        #[usage(long, value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Start point for a newly created branch (default: current HEAD).
        #[usage(long, value_name = "REF")]
        from: Option<String>,

        /// Session name, overriding the name derived from the path.
        #[usage(long, short = 's', value_name = "NAME")]
        session: Option<String>,

        /// Path inside the repository the worktree belongs to.
        #[usage(long, default = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Attach to the new session instead of creating it headlessly.
        #[usage(long)]
        attach: bool,

        /// Emit a stable JSON document — branch, path, session, and the seed
        /// pane's `terminal_id` — instead of human text. This is the first
        /// call in a fan-out script, and the id it returns is the pane the
        /// caller then sends its first prompt to. Cannot combine with
        /// `--attach`: an attached session owns stdout.
        #[usage(long, conflicts("--attach"))]
        json: bool,

        /// Command to run in the new session instead of the default shell.
        #[usage(trailing_var_arg)]
        command: Vec<String>,
    },

    /// Open the session bound to an existing worktree, creating it if absent.
    ///
    /// Idempotent: an already-live session is reported and left alone, so
    /// scripts and keybindings can call this without checking first.
    Open {
        /// Worktree path, branch, or derived session name.
        target: String,

        /// Path inside the repository the worktree belongs to.
        #[usage(long, default = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Attach to the session instead of only reporting its name.
        #[usage(long)]
        attach: bool,

        /// Emit the same document `worktree new --json` emits, whether the
        /// session was created now or was already live — so a script that
        /// re-enters a fleet gets the seed pane without special-casing.
        #[usage(long, conflicts("--attach"))]
        json: bool,
    },

    /// Remove a worktree, killing the session bound to it first.
    ///
    /// The session is killed before git runs, because git refuses to remove
    /// a worktree whose files are held open and a shell sitting in that
    /// directory holds it open. Refuses the worktree you are standing in.
    #[usage(alias = "rm")]
    Remove {
        /// Worktree path, branch, or derived session name.
        target: String,

        /// Pass --force to git, removing a worktree with local changes.
        #[usage(long)]
        force: bool,

        /// Path inside the repository the worktree belongs to.
        #[usage(long, default = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text. A fan-out
        /// teardown script has the same parsing problem creation does.
        #[usage(long)]
        json: bool,
    },
}
