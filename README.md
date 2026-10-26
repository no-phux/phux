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

**macOS:** download the **macos-arm64.dmg** from the latest
[Desktop release](https://github.com/no-phux/phux/releases?q=desktop-v&expanded=true).
Drag **Phux.app** to **Applications**, eject the image, and open Phux.
Stable downloads are Developer ID signed and Apple notarized. The app includes
the CLI/server for first launch; an existing CLI is never replaced.

**macOS or Linux:** prefer a shell installer?

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

### Platform support

| Platform | CLI / server / terminal UI | Desktop |
| --- | --- | --- |
| macOS, Apple silicon | Native | macOS 26+; no Xcode needed to install |
| Linux, x86_64 or arm64 | Native, glibc | X11 or Wayland, Vulkan driver |
| Windows | WSL 2; optional PowerShell launchers | Native x64 source build, WSL 2 coordinator |

No Intel Mac build, musl build, or native Windows server is published.
See [installation options and requirements](./docs/install.md#requirements),
[Windows setup](./docs/install.md#windows-wsl), and
[desktop setup](./clients/desktop/README.md).

## Use

```sh
phux                      # start the server, attach; Ctrl-A is the prefix, Ctrl-A d detaches
phux ls                   # what is running
phux snapshot @1          # a pane's screen, from any shell
phux run @1 'cargo test'  # run a command in it, get the output and exit code
phux wait --regex 'passed|failed' @1
```

[Getting started](./docs/getting-started.md) ·
[driving terminals from scripts](./docs/scripting.md) ·
[coding agents](./docs/agents.md) ·
[remote machines](./docs/remote.md)

## For agents

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

The CLI and terminal UI are the parts in daily use. The desktop app is
released and rough. The browser client is a starting point, not a product.
Phone apps are in progress elsewhere. The wire protocol is `0.x` and
changes additively; see [protocol](./docs/protocol.md).

## Contributing

[CONTRIBUTING.md](./CONTRIBUTING.md), then [development](./docs/development.md).
phux is picky about new surface area; ask before building a feature.

## Help

- **Something broken?** Run `phux doctor`, then see [troubleshooting](./docs/troubleshooting.md).
- **Desktop startup?** See [first launch and recovery](./clients/desktop/README.md#first-launch-and-recovery); the palette's **Copy Diagnostics** gathers useful details.
- **Questions and setup help:** [Discord](https://discord.gg/dUv5rzdHp).
- **Reproducible bug or feature proposal:** [GitHub issues](https://github.com/no-phux/phux/issues). See [what to include](./docs/troubleshooting.md#reporting-a-bug); review logs for secrets first.
- **Building from source:** [development setup](./docs/development.md#gpui-desktop), including macOS 26+ / Xcode 26+.

## License

[Apache-2.0](./LICENSE). [NOTICE](./NOTICE) ·
[third-party notices](./THIRD-PARTY-NOTICES.md)
