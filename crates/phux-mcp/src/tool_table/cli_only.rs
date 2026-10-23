//! The agent-facing CLI verbs with no MCP tool, and why.

/// CLI verbs on the `docs/agents.md#commands-agents-use` list that have
/// no MCP tool, each with its reason. The parity gate requires
/// every agent-facing verb there to have a tool or a row here.
pub(crate) const CLI_ONLY: &[(&str, &str)] = &[
    (
        "config distro list",
        "local distro package inventory; no server action",
    ),
    (
        "config distro current",
        "local configured selection; no server action",
    ),
    (
        "config distro use",
        "operator config-file selection; reload is explicitly separate",
    ),
    (
        "config distro off",
        "operator config-file selection; reload is explicitly separate",
    ),
    (
        "logs",
        "local operator log inventory and tailing, not a model-facing server action",
    ),
    (
        "update",
        "replaces the operator's installed binaries; installation management stays outside MCP",
    ),
    (
        "channel",
        "changes the operator's installed release channel; installation management stays outside MCP",
    ),
    (
        "config agents",
        "local config inventory of declared agent integrations; `phux_agent_list` covers the live agents",
    ),
    // Host enrollment and renewal remain CLI-only operator workflows.
    (
        "host add",
        "operator host-registry enrollment, not an agent action",
    ),
    (
        "host ls",
        "operator inventory of the host registry, not an agent action",
    ),
    (
        "host discover",
        "operator tailnet inventory for enrollment, not an agent action",
    ),
    (
        "host renew",
        "operator host-registry certificate renewal, not an agent action",
    ),
    (
        "host upgrade",
        "operator host-registry transport upgrade, not an agent action",
    ),
    (
        "pair",
        "mints a pairing secret; credential handling stays outside the model-facing set",
    ),
    ("mcp", "launches this adapter itself"),
    (
        "deny",
        "`phux_approve` decides both ways: `decision: deny` is `phux deny`",
    ),
    ("resize", "not exposed over MCP yet; a known parity gap"),
    (
        "restore",
        "an operator's after-restart sweep; an agent relaunches the one pane it means with \
         `phux_relaunch`",
    ),
    (
        "rec",
        "not exposed over MCP yet; a recording is written to a file on the adapter's host",
    ),
    (
        "play",
        "not exposed over MCP yet; playback replays a file from the adapter's host",
    ),
    (
        "rec mark",
        "not exposed over MCP yet; it marks recordings written on the adapter's host",
    ),
];
