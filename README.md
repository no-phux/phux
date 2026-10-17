<!--
audience: humans, contributors, agents
stability: stable
last-reviewed: 2026-10-13
-->

<p align="center">
  <img src="docs/assets/fox-mark.svg" alt="phux" width="128">
</p>

# phux

part of [no-phux](https://github.com/orgs/no-phux/repositories)

[Docs](https://docs.phux.sh/overview) · [Discord](https://discord.gg/dUv5rzdHp)
[![CI](https://github.com/no-phux/phux/actions/workflows/ci.yml/badge.svg)](https://github.com/no-phux/phux/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

## terminals as objects on a wire

phux keeps each shell running on a server, so a terminal is an object you can
split, leave, and attach to again. People running pi or Claude Code can put
the agent in one pane and point it at another: the pi extension and the
Claude Code plugin can read that pane and type a command into the live
shell, and the shell is still there after you detach. Walk through a pi
session in [pi + phux in 60 seconds](./docs/consumers/pi-in-60-seconds.md).

<!--
TODO: iOS availability wording. Patrick has not chosen between a TestFlight
link and "coming soon". Keep the next line neutral. Do not add a TestFlight
URL or a coming-soon claim until that choice is made.
-->

The [iOS client page](./docs/consumers/ios.md) lists integration requirements
and does not establish release availability.

phux is a programmable terminal runtime. A background server keeps shells
running; public control and event interfaces let people, scripts, and coding
agents work with them across clients and machines.

- Split, detach, and reattach in the TUI, or use a native or browser client.
- Read terminal state, send input, and wait for output through the JSON CLI,
  SDK, or MCP adapter. Visual and headless clients use the same resource protocol.
- Read structured agent lifecycle records from a harness, with terminal
  detection as the fallback.
- Connect directly to remote machines: `phux --remote me@mini` pairs over SSH
  once, then attaches over QUIC. No phux account is required.

## tmux, Herdr, or phux?

Use tmux for mature terminal multiplexing. Choose phux when you also need
public terminal and agent-event interfaces, independent clients, or direct
remote attachment.

Use Herdr for an integrated agent workspace. Both systems keep real PTYs
alive and expose agent-aware control. Herdr projects its workspace model to
its clients; phux exposes Terminal and AgentSession resources for clients
with their own layouts and workflows.

[Choose by use case](./docs/when-to-use.md) ·
[translate tmux keys](./docs/coming-from.md) ·
[compare the Herdr architecture](./docs/architecture/phux-and-herdr.md) ·
[see measured performance](./docs/performance.md)

## Install

```sh
curl -fsSL https://phux.sh/install | sh
```

Or with Homebrew:

```sh
brew trust --tap no-phux/tap
brew install no-phux/tap/phux
```

Release builds support macOS arm64, Linux x86_64, and Linux arm64. Windows is
not supported. The macOS and Linux desktop app installs the CLI with it:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

Run `phux` to start. Prefix is `Ctrl-A`; `Ctrl-A d` detaches. `phux update`
keeps it current. Options and source builds:
[Install](./docs/INSTALL.md). The Zig Cockpit is [archived](./docs/consumers/cockpit.md).

## First minute

```sh
phux                       # start the server and attach
# Ctrl-A %                 # split left/right
# Ctrl-A d                 # detach; the shells keep running
phux ls --json             # inspect the same terminals headlessly
phux snapshot default      # read the current grid without attaching a UI
```

[Quickstart](./docs/QUICKSTART.md) ·
[pi + phux in 60 seconds](./docs/consumers/pi-in-60-seconds.md) ·
[keys and configuration](./docs/CONFIG.md) ·
[remote access](./docs/remote-access.md) ·
[agent control](./docs/consumers/agents.md) ·
[harness emit contract](./docs/consumers/harness.md) ·
[build a client](./docs/consumers/build-a-client.md)

## Get your agent started

Give this to your coding agent:

```text
Read https://docs.phux.sh/llms.txt and help me get started with phux.
Choose the setup for my agent host and verify a read before sending input.
Ask before interrupting or destroying existing work.
```

Install the operating skills, then [connect your agent host](./docs/consumers/getting-started.md):

```sh
npx skills add no-phux/skills
```

Skills teach operation; your host's native tools, the local MCP adapter, or
the CLI control your running terminals. The separate
[hosted docs MCP](./docs/consumers/getting-started.md#let-your-agent-read-the-docs)
only searches documentation and releases. The docs home has a copyable agent
prompt, and every documentation page has a direct Markdown URL.

## License

[Apache-2.0](./LICENSE). Copyright 2026 phall.

[NOTICE](./NOTICE) · [Third-party notices](./THIRD-PARTY-NOTICES.md)
