<!--
audience: humans, contributors, agents
stability: stable
last-reviewed: 2026-09-13
-->

# phux

part of [no-phux](https://github.com/orgs/no-phux/repositories)

[Docs](https://docs.phux.sh/overview) · [Discord](https://discord.gg/dUv5rzdHp)
[![CI](https://github.com/no-phux/phux/actions/workflows/ci.yml/badge.svg)](https://github.com/no-phux/phux/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

A terminal multiplexer. Your shells live in a background server. Split,
detach, they keep running. The TUI, Cockpit, a script, and an agent all
attach to the same live terminal.

- **same objects** — the pane you are looking at is the pane the harness drives.
- **blocked is a fact** when the harness emits. Screen detection is the fallback.
- **another machine, no account** — `phux --remote me@mini` pairs once; later
  dials are QUIC. There is no phux account in the path.

```sh
curl -fsSL https://phux.sh/install | sh
phux
```

Prefix is `Ctrl-A`. `Ctrl-A d` detaches. Homebrew and other channels:
[Install](./docs/INSTALL.md).

## Coming from

| You used | |
|---|---|
| **tmux** | Same attach, split, and prefix muscle memory. Every pane is also a real terminal an agent can read and type into. |
| **old phux `herdr` distro** | Not herdr.dev. Those defaults are stock phux. `phux config init --distro starter` for the demo plugins. |
| **screen** | Attach and detach. The rest is in the docs. |

Longer translation: [Coming from tmux, screen, or the old phux distro](./docs/coming-from.md).

Keys, remote, agents, Cockpit, the wire: [docs.phux.sh](https://docs.phux.sh/overview).
Harness authors: [emit contract](./docs/consumers/harness.md).
New clients: [build against the wire](./docs/consumers/build-a-client.md).

## License

[Apache-2.0](./LICENSE). Copyright 2026 phall.

[NOTICE](./NOTICE) · [Third-party notices](./THIRD-PARTY-NOTICES.md)
