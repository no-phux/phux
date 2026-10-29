# All docs

Every page in the phux docs, grouped by what you're trying to do.

## Get started

- [Install](./install.md): the install script, Homebrew, the desktop app, updating, and removing phux.
- [Getting started](./getting-started.md): your first session, the keys that matter, detaching, and coming from tmux.
- [Concepts](./concepts.md): the server, sessions, tabs, panes, clients, hosts, targets, and what survives a restart.
- [phux vs tmux, Herdr, and cmux](./compare.md): which one fits the job.

## Customize

- [Configuration file](./config.md): where the file lives, how it layers over the defaults, and when a change takes effect.
- [Key bindings](./keys.md): change the prefix, rebind or unbind keys, and see what's in effect.
- [Themes](./themes.md): install, pick, and write colour themes.
- [Status bar and sidebar](./status-bar.md): what the bar and the sidebar show, and how to change it.
- [Hooks](./hooks.md): run a command when something happens.
- [Plugins](./plugins.md): add actions, panes, and sidebar sections.
- [Distributions](./distros.md): switch curated defaults without replacing your config; run the Crew agent distro.
- [Omarchy](./omarchy.md): one command to fit phux into an Omarchy desktop, with its keys, theme, and launcher.
- [Projects](./projects.md): open a project's session from a recipe in its repository.

## Automate

- [Using phux as an agent](./agent-quickstart.md): install phux and drive sessions, panes, input, and scrollback from code.
- [Driving terminals from scripts](./scripting.md): snapshot, send keys, run, wait, with JSON output and exit codes.
- [Coding agents](./agents.md): see which agent needs you, connect your agent's host, and use the MCP server.
- [ACP sessions](./acp.md): run an agent through the Agent Client Protocol, and answer its permission requests yourself.

## Build for phux

- [Program protocol](./program-protocol.md): how a program in a pane reports what it is doing with OSC 7501, and what phux shows.
- [Any program can report status](./any-program-status.md): a short OSC 7501 emitter, a demo with no hooks, and how phux ranks that report.
- [Program sequences and agent reports](./program-sequences.md): titles, `phux-ask` questions, OSC 133, OSC 52, and the other sequences phux reads, plus the agent record.

## Machines and clients

- [Remote machines](./remote.md): reach another machine's server with `phux host add`, and link machines together.
- [Clients](./clients.md): the terminal UI, the desktop app, the browser client, and phones.
- [Desktop Quick Capture](./desktop-capture.md): start an agent from a thought without leaving what you're doing.
- [Desktop Changes panel](./desktop-changes.md): review, stage, and commit the focused checkout.
- [Desktop Reviews](./desktop-reviews.md): review, comment on, and merge your open GitHub pull requests.
- [Desktop agent panes](./desktop-panes.md): how the desktop app shows agent panes.

## Help

- [Troubleshooting](./troubleshooting.md): what to run first, and the failures people hit.
- [Supported platforms](./install.md#requirements) and [desktop installation](./install.md#desktop-app): choose the right download.
- [Desktop first launch and recovery](../clients/desktop/README.md#first-launch-and-recovery): bundled runtime, retry, and startup failures.
- [Report a bug](./troubleshooting.md#reporting-a-bug): gather diagnostics and remove private information before sharing.
- [Discord](https://discord.gg/dUv5rzdHp): questions and setup help.
- [GitHub issues](https://github.com/no-phux/phux/issues): reproducible bugs and feature proposals.

## Reference

Generated from the `phux` binary, so they match the code these docs describe.

- [All reference pages](./reference/README.md): what each generated page covers.
- [CLI](./reference/cli.md): the `--help` text of every command.
- [Configuration settings](./reference/config.md): every key in the config file, with its default.
- [Default key bindings](./reference/keys.md): every key phux binds out of the box.
- [Actions](./reference/actions.md): everything a key binding can run.
- [Status bar widgets](./reference/widgets.md): the pieces the status bar is built from, and their options.
- [Hook events](./reference/hooks.md): every event a hook can run on.
- [Environment variables](./reference/environment.md): what phux reads and sets.
- [Exit codes](./reference/exit-codes.md): what each exit status means, for scripts.
- [File locations](./reference/files.md): the socket, config, state, and logs.
- [Resource kinds](./reference/kinds.md), [MCP tools and the commands they mirror](./reference/parity.md), and [deprecations](./reference/deprecations.md).

## Work on phux

- [Architecture](./architecture.md): the process model, crates, and clients.
- [Decisions](./decisions.md): the constraints the code keeps, and why.
- [Organization](./organization.md): how the server owns sessions and their contents.
- [Substrate](./substrate.md): how people, hosts, and agents fit together.
- [Protocol](./protocol.md): framing, handshake, capabilities, and changing the wire.
- [Host services](./host-services.md): HTTP to a host service (OpenCode) over the phux connection.
- [Compatibility](./compatibility.md): what an older or newer peer sees.
- [Embedding](./embedding.md): put a phux terminal in your own app.
- [Development](./development.md): set up, build, and run the checks.
- [Releasing](./releasing.md): what ships, and how.

These docs track `main`. `phux --help` describes the version you have installed.
