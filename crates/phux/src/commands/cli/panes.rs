//! The `Panes` verbs that place, move, and label panes.

use crate::commands::{JsonOpt, SpawnSplit, resize};
use usage::{Args, Subcommands};

/// Create a pane without attaching
///
/// With `--target`, the pane is inserted beside an exact local owner;
/// otherwise it joins the server's most recently active session. The new
/// pane's id prints to stdout. With `--satellite NAME` on a
/// federation hub (`phux server --hub`), the spawn is routed over
/// the hub's link to that satellite and the returned id is
/// qualified with that host — addressable through the hub by every
/// satellite-capable verb. Does not auto-start a server.
#[derive(Debug, Args)]
pub(crate) struct SpawnArgs {
    /// Route the spawn to a configured federation satellite (a name
    /// from `phux host ls --role satellite`, on a server running
    /// `--hub`).
    #[usage(long, value_name = "NAME")]
    pub(crate) satellite: Option<String>,

    /// Existing local pane beside which to place the new pane.
    #[usage(long, value_name = "TARGET", conflicts("--satellite"))]
    pub(crate) target: Option<String>,

    /// Placement axis: `horizontal` puts the new pane to the right;
    /// `vertical` puts it below (requires `--target`).
    #[usage(long, value_enum, default = "horizontal", requires("--target"))]
    pub(crate) split: SpawnSplit,

    /// Fraction of the split retained by TARGET (requires `--target`).
    #[usage(
        long,
        default = "0.5",
        default_value_t = 0.5,
        requires("--target"),
        validate = "float(value) > 0 && float(value) < 1",
        validate_error = "ratio must be finite and strictly between 0 and 1"
    )]
    pub(crate) ratio: f32,

    /// Named projection to place into instead of the shared default
    /// (requires `--target`). Must be
    /// `<prefix>.layout/v1/<session-id>` for TARGET's session.
    #[usage(long, value_name = "KEY", requires("--target"))]
    pub(crate) projection: Option<String>,

    /// Working directory for the new pane.
    #[usage(short = 'c', long = "cwd")]
    pub(crate) cwd: Option<String>,

    /// Keep the pane inspectable after its process exits, for SECS
    /// seconds (bare `--retain`: the server's default). Its screen,
    /// history, and exit status stay readable through `phux resource
    /// show` and `phux resource wait` until then, or until `phux kill`.
    /// Write `--retain=SECS` when a command follows.
    #[usage(long, value_name = "SECS", num_args = 0..=1, default_missing = "0")]
    pub(crate) retain: Option<u32>,

    /// Make the spawn safe to retry: a repeat with the same key (32 hex
    /// digits) and the same request answers the first pane instead of
    /// spawning another. Draw one key per spawn and reuse it on retry.
    #[usage(long = "idempotency-key", value_name = "HEX32")]
    pub(crate) idempotency_key: Option<String>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    /// Command (and arguments) to run instead of the default shell.
    /// Must follow `--`: `phux spawn -- htop`.
    #[usage(trailing_var_arg)]
    pub(crate) command: Vec<String>,
}

/// Insert an existing pane into a layout
///
/// Both selectors must each resolve to exactly one local pane in the same
/// session. This command does not spawn: create `NEW_PANE` first with
/// `phux spawn`, then insert it. Omitted direction defaults horizontal.
#[derive(Debug, Args)]
pub(crate) struct InsertPaneArgs {
    /// Existing layout leaf beside which `NEW_PANE` is inserted.
    pub(crate) target: String,
    /// Already-created pane to insert; no implicit spawn occurs.
    pub(crate) new_pane: String,
    /// Split axis: `horizontal` stacks the panes, `vertical` places
    /// them side-by-side.
    #[usage(long, value_enum, default = "horizontal")]
    pub(crate) split: SpawnSplit,
    /// Fraction assigned to TARGET; must be strictly between 0 and 1.
    #[usage(
        long,
        default = "0.5",
        default_value_t = 0.5,
        validate = "float(value) > 0 && float(value) < 1",
        validate_error = "ratio must be finite and strictly between 0 and 1"
    )]
    pub(crate) ratio: f32,
    /// Named projection to edit instead of the shared default
    /// (`phux.tui.layout/v1/<session>`). Must be
    /// `<prefix>.layout/v1/<session-id>` for the session both selectors
    /// resolve to; at most one may be given.
    #[usage(long, value_name = "KEY")]
    pub(crate) projection: Vec<String>,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Move a pane into another session, or beside another pane
///
/// When DEST names a session (`work`, or a session key `^ab23cd`), SOURCE
/// becomes a member of it and nothing else changes: its process,
/// scrollback, and id survive, and it is not placed in the destination's
/// layout. Otherwise DEST is a pane: SOURCE is collapsed out of its tree
/// position and inserted beside it, re-parented across sessions first
/// when needed.
#[derive(Debug, Args)]
pub(crate) struct MvArgs {
    /// Pane to move.
    pub(crate) source: String,
    /// Destination session, or the pane to place SOURCE beside.
    pub(crate) dest: String,
    /// Split axis beside a destination pane: `horizontal` stacks the
    /// panes, `vertical` places them side-by-side.
    #[usage(long, value_enum, default = "horizontal")]
    pub(crate) split: SpawnSplit,
    /// Fraction assigned to a destination pane; must be strictly between
    /// 0 and 1.
    #[usage(
        long,
        default = "0.5",
        default_value_t = 0.5,
        validate = "float(value) > 0 && float(value) < 1",
        validate_error = "ratio must be finite and strictly between 0 and 1"
    )]
    pub(crate) ratio: f32,
    /// Named projection(s) to edit instead of the shared default, for a
    /// move beside a pane. A same-session move takes at most one KEY; a
    /// cross-session move takes two (source and destination) or none.
    #[usage(long, value_name = "KEY")]
    pub(crate) projection: Vec<String>,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Swap two panes in a layout
///
/// Both selectors must each resolve to exactly one local pane. Split
/// geometry is preserved and attached clients retain their local focus.
#[derive(Debug, Args)]
pub(crate) struct SwapPaneArgs {
    /// First pane selector.
    pub(crate) first: String,
    /// Second pane selector.
    pub(crate) second: String,
    /// Named projection to edit instead of the shared default;
    /// at most one may be given.
    #[usage(long, value_name = "KEY")]
    pub(crate) projection: Vec<String>,
    /// Emit a schema-versioned JSON result or error.
    #[usage(long)]
    pub(crate) json: bool,
}

/// Set a pane's grid size, with no TTY.
#[derive(Debug, Args)]
#[usage(
    about = "Set a pane's grid size",
    long_about = "Set a pane's grid size, with no TTY.\n\n\
            The headless counterpart to resizing your terminal window: names one \
            pane and gives it an exact cell geometry. Nothing attaches and \
            nothing subscribes, so the pane is never dragged toward the 80x24 \
            size a program with no terminal would otherwise report.\n\n\
            The new size takes effect immediately, even with someone attached. \
            It is not permanent against an attached view: under the default \
            `window-size = \"smallest\"` policy the next attach, detach, or window \
            resize recomputes the pane's geometry from the attached views and \
            overrides it. Set `window-size = \"manual\"` when an explicit size \
            must hold. Either way this verb reads the server's real size back \
            before exiting, and exits nonzero if it is not the one you asked \
            for, so a script can never mistake a delivered request for an \
            applied one.\n\n\
            Examples:\n  \
            phux resize demo 120x40\n  \
            phux resize @7 200x50 --json"
)]
pub(crate) struct ResizeArgs {
    /// Target selector: session, session:group, session:group.pane,
    /// @id, or `.` (focused). `=` is unsupported by headless commands.
    pub(crate) target: String,

    /// New grid size, e.g. 120x40. Both axes are whole numbers of
    /// cells and at least 1.
    #[usage(value_name = "COLSxROWS")]
    pub(crate) geometry: resize::Geometry,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Create and place terminals; inspect API and receipts
///
/// Always returns versioned JSON. Creation commits membership, placement and
/// launch intent together; a receipt is not process or task completion.
#[derive(Debug, Args)]
pub(crate) struct TerminalArgs {
    #[usage(subcommand)]
    pub(crate) action: TerminalAction,
}

/// `phux terminal <action>` — the complete-intention terminal workflow.
#[derive(Debug, Subcommands)]
pub(crate) enum TerminalAction {
    /// Recover a retained creation receipt without executing or retrying anything.
    /// A missing receipt means unknown, not that the original request was refused.
    Operation {
        /// Original full durable destination session key.
        #[usage(long, value_name = "KEY")]
        session: String,
        /// Same key supplied to terminal create.
        #[usage(long = "idempotency-key", value_name = "HEX32")]
        idempotency_key: String,
    },
    /// Read the connected server's public workflow schemas and retry contracts.
    Describe,
    /// Create, place and start a terminal from one strict JSON intention.
    ///
    /// Use `terminal describe` for the complete nested request schema.
    /// Argv runs in a new process, never by typing into an existing shell.
    /// The resource and failed/completed execution remain until explicit removal.
    Create {
        /// JSON intention containing `idempotency_key`, session and optional argv/cwd/env/name/placement.
        #[usage(long = "request-json", value_name = "JSON")]
        request_json: String,
    },
}

/// Inspect a resource or wait for its process to exit
///
/// `show` reads one resource: kind, parent, lifecycle, how a retained
/// process exited, its process facts, input holder, tags, and agent
/// record. `wait` blocks until the resource's process ends (exit 0),
/// reports a resource that is already gone (exit 1), or gives up at
/// `--timeout` (exit 124), and prints a cursor that resumes it. `methods`
/// lists what the resource answers on this server. None of them attaches
/// or resizes.
#[derive(Debug, Args)]
pub(crate) struct ResourceArgs {
    #[usage(subcommand)]
    pub(crate) action: ResourceAction,
}

/// Read and write pane tags (address them with #tag)
///
/// Tags are freeform strings attached to panes. Once a pane is tagged,
/// the `#tag` selector addresses every pane carrying that tag — e.g.
/// `phux kill #build`, `phux snapshot #web`.
#[derive(Debug, Args)]
pub(crate) struct TagArgs {
    #[usage(subcommand)]
    pub(crate) action: TagAction,
}

/// `phux resource <action>` — one resource's record, its methods, and a
/// resumable wait on its exit.
#[derive(Debug, Subcommands)]
pub(crate) enum ResourceAction {
    /// Show one resource: kind, lifecycle, exit, process, tags, agent.
    ///
    /// Prints `key: value` lines, or with `--json` the stable document.
    /// Read-only: never attaches or resizes.
    Show {
        #[usage(flatten)]
        json: JsonOpt,

        /// Target selector. A direct id (`@N`, `host/@N`) is read as given.
        #[usage(value_name = "TARGET")]
        target: String,
    },

    /// Wait until a resource's process exits.
    ///
    /// Exit 0 when it exited (now or earlier, while the server still holds
    /// it), 1 when it is gone (it closed unretained, or never existed), 124
    /// at `--timeout`, 2 for a bad argument. Race-free: an exit that happens
    /// while the wait starts is never missed. With `--json` the document
    /// carries the outcome, the exit status or signal, and a `cursor`; pass
    /// that cursor to `--after` to resume after a disconnect.
    Wait {
        /// Give up after this many seconds (exit 124), counted from the start
        /// of the command. Default: wait forever.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Resume from the cursor a previous `resource wait` or `watch`
        /// printed: a close the server still holds is replayed instead of
        /// read as gone. A cursor from another server run is ignored.
        #[usage(long, value_name = "CURSOR")]
        after: Option<String>,

        #[usage(flatten)]
        json: JsonOpt,

        /// Target selector. A direct id (`@N`) is used as given, so a
        /// resource that already exited can still be waited on.
        #[usage(value_name = "TARGET")]
        target: String,
    },

    /// List the methods a resource answers on this server.
    ///
    /// Each method's verb, whether it changes state, and whether it is
    /// available here: a method another kind owns, or one that needs a
    /// feature this server does not advertise, is listed with the reason.
    /// Listing a method grants nothing.
    Methods {
        #[usage(flatten)]
        json: JsonOpt,

        /// Target selector. A direct id (`@N`, `host/@N`) is read as given.
        #[usage(value_name = "TARGET")]
        target: String,
    },
}

/// `phux tag <action>` — list and edit a Terminal's L3 tags.
///
/// Alias policy (ADR-0065 §5): every list/remove registry verb answers to
/// both spellings. This registry's canonical names were the short ones, so
/// the aliases here are the long forms.
#[derive(Debug, Subcommands)]
pub(crate) enum TagAction {
    /// List the tags on each pane a selector resolves to.
    #[usage(alias = "list")]
    Ls {
        /// Target selector (session, `session:group`, `@id`, `.`, `#tag`).
        target: String,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Add one or more tags to each pane a selector resolves to.
    Add {
        /// Target selector.
        target: String,
        /// Tags to add (the leading `#` is optional).
        #[usage(required)]
        tags: Vec<String>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Remove one or more tags from each pane a selector resolves to.
    #[usage(alias = "remove")]
    Rm {
        /// Target selector.
        target: String,
        /// Tags to remove (the leading `#` is optional).
        #[usage(required)]
        tags: Vec<String>,

        #[usage(flatten)]
        json: JsonOpt,
    },
}
