# phux docs

Short pages, in the order most people need them. Anything the binary can
state for itself (commands, settings, environment variables, exit codes,
wire frames) is generated under [reference](./reference/README.md) and
[spec](./spec/wire-schema.json), so the pages here link to it instead of
repeating it.

## Source and installed releases

These live guides and generated references track the **main-source revision**,
which may include behavior not yet released. The same Cargo version string
does not prove that an installed binary contains a command from this tree.
Use the installed command's `--help` and skill output (`phux skill`, or
`--skill` where supported), and check the running server's
[feature availability](./protocol.md#capabilities-not-versions). Compare
source commit SHA/build provenance, not SemVer alone, when reporting a
mismatch. A missing command is not a reason to point a checkout build at
production state or install an unreleased build.

## Use phux

- [Install](./install.md): installer, Homebrew, desktop app, updating, from source.
- [Getting started](./getting-started.md): first session, keys, detaching, coming from tmux.
- [Concepts](./concepts.md): the server, sessions and panes, targets, what survives what.
- [Configuration](./config.md): keys, themes, status bar, hooks, plugins, projects.
- [Driving terminals from scripts](./scripting.md): snapshot, send keys, run, wait, JSON and exit codes.
- [Coding agents](./agents.md): agent state, host integrations, MCP, safety.
- [Remote machines](./remote.md): `phux host add`, what is exposed, relays, linking machines.
- [Clients](./clients.md): terminal UI, desktop, browser, phones.
  - [Quick Capture](./desktop-capture.md): launch from a thought without losing keyboard focus.
  - [Desktop Changes](./desktop-changes.md): review, stage, and commit the focused checkout.
  - [Desktop agent panes](./desktop-panes.md): identity, harness marks, and stable geometry.
- [Embedding](./embedding.md): GPUI, other Rust toolkits, C and Swift.
- [Troubleshooting](./troubleshooting.md): doctor, logs, known failures.

## Reference

- [CLI](./reference/cli.md), [config](./reference/config.md), [actions](./reference/actions.md), [widgets](./reference/widgets.md), [hooks](./reference/hooks.md)
- [environment variables](./reference/environment.md), [exit codes](./reference/exit-codes.md), [files](./reference/files.md), [deprecations](./reference/deprecations.md)
- [resource kinds](./reference/kinds.md), [MCP tools](./reference/parity.md), [wire schema](./spec/wire-schema.json)

## Work on phux

- [Architecture](./architecture.md): process model, crates, clients, the decisions behind them.
- [Organization](./organization.md): durable records, transaction ordering, execution and restore authority.
- [Protocol](./protocol.md): framing, handshake, capabilities, replica recovery and bounded retries.
- [Development](./development.md): environments, build cache, per-area setup, gates, CI.
- [Releasing](./releasing.md): what ships, how, and the manual controls.
- [Contributing](../CONTRIBUTING.md) and [agent instructions](../AGENTS.md).
