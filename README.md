# phux

A terminal multiplexer whose server you can drive from code.

phux keeps your shells running in a background server and shows them in a
terminal UI, like tmux. The difference is that everything the UI can do is
also a command with JSON output and an exit code, the server tracks which
panes are running a coding agent and whether it is waiting on you, and
another machine is one `phux host add me@mini` away, with no account.

[Docs](./docs/README.md) · [Discord](https://discord.gg/dUv5rzdHp) ·
[![CI](https://github.com/no-phux/phux/actions/workflows/ci.yml/badge.svg)](https://github.com/no-phux/phux/actions/workflows/ci.yml)

## Install

```sh
curl -fsSL https://phux.sh/install | sh
```

macOS on Apple silicon, Linux x86_64 and arm64. Also `brew install
no-phux/tap/phux`. [More options](./docs/install.md), including the desktop
app and building from source.

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

## License

[Apache-2.0](./LICENSE). [NOTICE](./NOTICE) ·
[third-party notices](./THIRD-PARTY-NOTICES.md)
