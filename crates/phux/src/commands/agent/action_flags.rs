//! Flag groups flattened into [`super::action::AgentAction`] variants.
//! Each struct owns exactly its verb's flags.

/// `phux agent list` filters, combined with AND.
#[derive(Debug, Default, usage::Args)]
pub(crate) struct ListFlags {
    /// Lifecycle state to include; repeat to OR several. Omit for all states.
    #[usage(
        long,
        value_name = "STATE",
        choices("unknown", "idle", "working", "blocked", "done")
    )]
    pub(crate) state: Vec<String>,
    /// Case-insensitive words matched across pane, session, agent, title and cwd.
    /// Every word must match; terminal contents are not searched.
    #[usage(long, value_name = "TEXT")]
    pub(crate) query: Option<String>,
    /// Emit machine-readable JSON instead of the table.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent native-command` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct NativeCommandFlags {
    /// Exact native activation from the session's structured view.
    #[usage(long)]
    pub(crate) activation: String,
    /// Nonzero command identity; retain it when querying an uncertain result.
    #[usage(long)]
    pub(crate) operation_id: u64,
    /// `NativeIntent` JSON: prompt, interrupt or answer with exact IDs.
    #[usage(long)]
    pub(crate) intent: String,
    /// Emit the actual command receipt as JSON.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent set` flags: one per agent record field.
#[derive(Debug, usage::Args)]
pub(crate) struct SetFlags {
    /// Human-facing agent name (required, non-empty).
    #[usage(long)]
    pub(crate) name: String,
    /// Open-vocabulary kind slug, e.g. "claude" or "codex".
    #[usage(long)]
    pub(crate) kind: Option<String>,
    /// Declared lifecycle state.
    #[usage(long, choices("unknown", "idle", "working", "blocked", "done"))]
    pub(crate) state: Option<String>,
    /// Declared attention priority (defaults derive from state).
    #[usage(long, choices("none", "low", "normal", "high"))]
    pub(crate) attention: Option<String>,
    /// Free-form association label (fleet/job name).
    #[usage(long)]
    pub(crate) session: Option<String>,
    /// Print the confirmed `{schema_version, action, terminal, record}`
    /// document; failures are one JSON error line on stderr.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent prompt` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct PromptFlags {
    /// Require the pane's declared agent name to be this one.
    #[usage(long, value_name = "NAME")]
    pub(crate) expect_agent: Option<String>,
    /// Require the pane's declared agent kind slug to be this one.
    #[usage(long, value_name = "KIND")]
    pub(crate) expect_kind: Option<String>,
    /// After delivering, block until the agent transitions into a
    /// lifecycle state.
    #[usage(long)]
    pub(crate) wait: bool,
    /// Lifecycle state to wait for; repeat to OR several. Defaults to
    /// `idle`, `blocked`, `done`. Requires `--wait`.
    #[usage(
        long,
        value_name = "STATE",
        choices("idle", "working", "blocked", "done")
    )]
    pub(crate) until: Vec<String>,
    /// Give up waiting after this many seconds and exit 124. The prompt
    /// was still delivered. Requires `--wait`.
    #[usage(long, value_name = "SECS")]
    pub(crate) timeout: Option<u64>,
    /// Emit the machine-readable result document instead of staying
    /// quiet on success.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent send-keys` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct SendKeysFlags {
    /// Require the pane's declared agent name to be this one.
    #[usage(long, value_name = "NAME")]
    pub(crate) expect_agent: Option<String>,
    /// Require the pane's declared agent kind slug to be this one.
    #[usage(long, value_name = "KIND")]
    pub(crate) expect_kind: Option<String>,
    /// Emit machine-readable JSON instead of staying quiet on success.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent answer` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct AnswerFlags {
    /// The id of the ask being answered, as carried by the `asked`
    /// event. Required: answering "whatever is being asked right now" is
    /// a level read, and a level read cannot tell one question from the
    /// next.
    #[usage(long, value_name = "ID")]
    pub(crate) id: String,
    /// Send the Nth published suggestion, 1-based, verbatim.
    #[usage(long, value_name = "N", conflicts("--text"))]
    pub(crate) choice: Option<usize>,
    /// Send exactly this text. Refused when the ask published a
    /// suggestion set and this is not in it (see `--allow-unlisted`).
    #[usage(long, value_name = "TEXT")]
    pub(crate) text: Option<String>,
    /// Permit a `--text` answer outside the ask's published suggestions.
    #[usage(long, requires("--text"))]
    pub(crate) allow_unlisted: bool,
    /// Emit machine-readable JSON instead of the one-line confirmation.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent start` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct StartFlags {
    /// Detection-manifest kind the started agent must identify as
    /// (`claude`, `codex`, ...). `phux agent explain --file` lists the
    /// loaded roster.
    #[usage(long, value_name = "KIND")]
    pub(crate) kind: String,
    /// Existing pane to start into. Never created, split, or moved.
    #[usage(long, value_name = "TARGET")]
    pub(crate) target: String,
    /// Launch integration id. Defaults to the unique enabled
    /// integration whose `[agent_identity] kind` matches `--kind`
    /// (so `--kind claude` resolves `claude-code`), else the kind
    /// slug itself; two claimants are refused by name.
    #[usage(long, value_name = "ID")]
    pub(crate) integration: Option<String>,
    /// Give up waiting for readiness after this many seconds and exit
    /// 124. The command was still typed.
    #[usage(long, value_name = "SECS")]
    pub(crate) timeout: Option<u64>,
    /// Submit and return without claiming readiness (exit 0,
    /// `ready: false`).
    #[usage(long)]
    pub(crate) no_wait: bool,
    /// Skip the available-shell precondition. Types the launch command
    /// into the pane whatever is running there.
    #[usage(long)]
    pub(crate) force: bool,
    /// Emit the machine-readable result document instead of a line.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent emit` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct EmitFlags {
    /// Record type, one of the closed `AgentEventsJsonlV1` set. A type
    /// outside it is refused as `record_invalid`, with nothing written.
    // An open string here so `EmitRecord::new` answers with the
    // machine-readable `record_invalid` document, not a usage error.
    #[usage(long = "type", value_name = "T")]
    pub(crate) event_type: String,
    /// Record payload: a JSON object inline, or `-` to read it from
    /// stdin. `{}` when omitted.
    #[usage(long, value_name = "JSON")]
    pub(crate) data: Option<String>,
    /// Emit the stamped record header as JSON instead of staying quiet.
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent log` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct LogFlags {
    /// Keep streaming live records after the retained ones.
    #[usage(long)]
    pub(crate) follow: bool,
    /// Return only the last N retained records.
    #[usage(long, value_name = "N")]
    pub(crate) tail: Option<usize>,
    /// Emit JSON: one envelope document, or under `--follow` one record
    /// per line with no envelope (the `watch --json` rule).
    #[usage(long)]
    pub(crate) json: bool,
}

/// `phux agent usage` flags.
#[derive(Debug, usage::Args)]
pub(crate) struct UsageFlags {
    /// Print the `phux.usage/v1` document instead of the human view.
    #[usage(long)]
    pub(crate) json: bool,
    /// Write the snapshot to the server's `phux.usage/v1` key.
    #[usage(long)]
    pub(crate) publish: bool,
    /// Ask every vendor now instead of reusing a reading under two
    /// minutes old (a rate-limit backoff still applies).
    #[usage(long)]
    pub(crate) refresh: bool,
}
