---
group: start
order: 9
slug: docs
---

# All docs

Every page, grouped by task. These docs track `main`; `phux --help` matches
the version you have.

## Get started

- [Install](./install.md): install, update, and remove phux and the desktop app.
- [Getting started](./getting-started.md): the first ten minutes.
- [Concepts](./concepts.md): server, sessions, panes, targets, and what survives a restart.
- [phux vs tmux, Herdr, and cmux](./compare.md): which one fits the job.

## Customize

- [Configuration file](./config.md): location, layers, reloads, and moving from another multiplexer.
- [Key bindings](./keys.md): change the prefix, rebind, and unbind.
- [Themes](./themes.md): install, pick, and write colour themes.
- [Status bar and sidebar](./status-bar.md): what they show and how to change it.
- [Hooks](./hooks.md): run a command when something happens.
- [Plugins](./plugins.md): actions, panes, and sidebar sections.
- [Maintained plugins](../plugins/README.md): the shipped plugin packages.
- [Distributions](./distros.md): curated defaults, including Omarchy.
- [Projects](./projects.md): open a project's session from a recipe in its repository.

## Automate

- [Using phux as an agent](./agent-quickstart.md): drive sessions and panes from code.
- [Scripting](./scripting.md): snapshot, send keys, run, wait, JSON output.
- [Coding agents](./agents.md): agent state, setup, the MCP server, and mail.
- [ACP sessions](./acp.md): run an agent over the Agent Client Protocol.
- [Examples](../examples/README.md): small runnable programs.

## Machines and clients

- [Remote machines](./remote.md): add hosts, pair devices, and link machines.
- [SSH](./ssh.md): sign-in, host keys, and port and agent forwarding.
- [Clients](./clients.md): the terminal UI, desktop app, native windows, and phones.
- [Security](./security.md): the audit log, held actions, credentials, and organizations.

## Build on phux

- [Build on phux](./building.md): the `--json` CLI, MCP, `phux watch`, and building a client.
- [Program Status Protocol](./program-protocol.md): report status from any program with OSC 7501.
- [Program sequences](./program-sequences.md): the other escape sequences phux reads.
- [Server protocol](./protocol.md): framing, handshake, and changing the wire.
- [Compatibility](./compatibility.md): what an older or newer peer sees.
- [File transfer and host services](./protocol-files.md): moving files and reaching host services.
- [Resource kinds](./resource-kinds.md): trees, surfaces, documents, mirrors, and applications.
- [Embed phux](./embedding.md): a phux terminal in your own app.

## Help

- [Troubleshooting](./troubleshooting.md): `phux doctor` and known failures.
- [Report a bug](./troubleshooting.md#reporting-a-bug): what to gather and redact.
- [Phux Rune diagnostics](../clients/rune/README.md#what-it-does): the desktop app's logs.
- [Discord](https://discord.gg/dUv5rzdHp): questions and setup help.
- [GitHub issues](https://github.com/no-phux/phux/issues): bugs and proposals.

## Reference

Generated from the `phux` binary.

- [All reference pages](./reference/README.md)
- [CLI](./reference/cli.md)
- [Configuration settings](./reference/config.md)
- [Default key bindings](./reference/keys.md)
- [Actions](./reference/actions.md)
- [Status bar widgets](./reference/widgets.md)
- [Hook events](./reference/hooks.md)
- [Environment variables](./reference/environment.md)
- [Exit codes](./reference/exit-codes.md)
- [File locations](./reference/files.md)
- [Resource kinds](./reference/kinds.md)
- [MCP parity](./reference/parity.md)

## Work on phux

- [Architecture](./architecture.md): process model, crates, clients, and rule ownership.
- [Architecture end state](./architecture-end-state.md): accepted implementation target and required qualification, not a shipping-status claim.
- [Decisions](./decisions.md): the constraints the code keeps.
- [Organization](./organization.md): how the server owns sessions.
- [Organization store](./organization-store.md): durability, receipts, and restart reconciliation.
- [Substrate](./substrate.md): people, hosts, and agents.
- [Authority](./authority.md): who may watch or control pages, windows, and displays.
- [libghostty](./libghostty.md): the terminal engine.
- [Design language](./design-language.md): tokens, themes, and brand assets.
- [Development](./development.md): set up, build, test.
- [CI](./ci.md): the lanes and gates.
- [Releasing](./releasing.md): what ships, and how.
