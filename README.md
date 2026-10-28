# phux

<img src="./docs/assets/logo.png" alt="Phux fox and wordmark" width="280">

A terminal multiplexer whose server you can drive from code.

phux keeps your shells running in a background server and shows them in a
terminal UI, like tmux. The difference is that everything the UI can do is
also a command with JSON output and an exit code, the server tracks which
panes are running a coding agent and whether it is waiting on you, and
another machine is one `phux host add me@mini` away, with no account.

[Docs](./docs/README.md) · [Help](#help) · [Releases](https://github.com/no-phux/phux/releases) ·
[![CI](https://github.com/no-phux/phux/actions/workflows/ci.yml/badge.svg)](https://github.com/no-phux/phux/actions/workflows/ci.yml)

## Install

### CLI and terminal UI

```sh
curl -fsSL https://phux.sh/install | sh
```

Or use [Homebrew](./docs/install.md#homebrew). Run `phux` after installing.

### Desktop app

**macOS:** [download the DMG](https://github.com/no-phux/phux/releases?q=desktop-v&expanded=true),
drag **Phux.app** to **Applications**, eject the image, and open Phux.
The app includes the CLI/server and is signed and notarized.
**Linux:** use the [desktop installer](./docs/install.md#desktop-app).

### Platform support

| Platform | CLI / server / terminal UI | Desktop |
| --- | --- | --- |
| macOS, Apple silicon | macOS 11+ | macOS 26+ |
| Linux, x86_64 or arm64 | Native, glibc | X11 or Wayland, Vulkan driver |
| Windows | [WSL 2 and PowerShell launchers](./docs/install.md#windows-wsl) | [Native x64, WSL coordinator](./clients/desktop/README.md#windows) |

[Other installation options](./docs/install.md). No Intel Mac or musl builds.

## Use

```sh
phux                      # start the server, attach; Ctrl-A is the prefix, Ctrl-A d detaches
phux ls                   # what is running
phux snapshot @1          # a pane's screen, from any shell
phux run @1 'cargo test'  # run a command in it, get the output and exit code
phux wait --regex 'passed|failed' @1
```

[Getting started](./docs/getting-started.md) ·
[using phux as an agent](./docs/agent-quickstart.md) ·
[driving terminals from scripts](./docs/scripting.md) ·
[coding agents](./docs/agents.md) ·
[remote machines](./docs/remote.md)

### Find your workflow

| Want to… | Start here |
| --- | --- |
| Find an agent across machines | [Desktop Go To](./clients/desktop/USAGE.md#command-palette), or [`phux agent list`](./docs/agents.md) |
| Run agents in isolated worktrees | [Missions](./docs/organization.md#work-and-missions) |
| Review, annotate and merge a pull request | [Desktop Reviews](./docs/desktop-reviews.md) |
| Inspect and stage local changes | [Changes panel](./docs/desktop-changes.md) |
| Work on another machine | [Remote machines](./docs/remote.md) |
| Script a terminal or expose agent tools | [Scripting](./docs/scripting.md) · [MCP](./docs/agents.md#mcp) |
| See agent plan usage and limits | [Plan usage](./docs/agents.md#plan-usage) |

## For agents

Install phux and drive a session from code with
[Using phux as an agent](./docs/agent-quickstart.md).

phux knows which pane runs which agent and whether it is working, idle, or
blocked on you. Agents reach phux through plugins for Claude Code, pi, Oh
My Pi, OpenCode, and Grok, through the bundled MCP server (`phux mcp`), or
through the CLI. To hand the manual to your agent:

```text
Run `phux skill` and follow it to work with my phux terminals.
```

Want one lead to coordinate a visible fleet? [Crew](./distros/crew/README.md)
is the bundled agent distro, with isolated homes, native missions and safe
[distro switching](./docs/distros.md).

## Status

The CLI and terminal UI are the parts in daily use. The desktop app (macOS, Linux) is new and changes weekly. The browser client is a starting point, not a product.
Phone apps are in progress elsewhere. The wire protocol is `1.x`; any 1.x
client works with any 1.x server. See [protocol](./docs/protocol.md).

## Contributing

[CONTRIBUTING.md](./CONTRIBUTING.md), then [development](./docs/development.md).
phux is picky about new surface area; ask before building a feature.

## Help

- **Troubleshooting:** run `phux doctor`, then see [common fixes](./docs/troubleshooting.md) or [desktop recovery](./clients/desktop/README.md#first-launch-and-recovery).
- **Questions:** [Discord](https://discord.gg/dUv5rzdHp).
- **Bugs and proposals:** [GitHub issues](https://github.com/no-phux/phux/issues); [what to include](./docs/troubleshooting.md#reporting-a-bug).

## License

[Apache-2.0](./LICENSE). [NOTICE](./NOTICE) ·
[third-party notices](./THIRD-PARTY-NOTICES.md)
