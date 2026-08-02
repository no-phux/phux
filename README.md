<!--
audience: humans, contributors, agents
stability: stable
last-reviewed: 2026-08-02
-->

<div align="center">

<img src="docs/assets/logo.svg" alt="phux" width="420">

# phux

**the tmux job, done - a terminal is an object on a wire**

[![CI](https://github.com/phall1/phux/actions/workflows/ci.yml/badge.svg)](https://github.com/phall1/phux/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

[Install](#install-and-run) |
[How it works](#how-it-works) |
[Keys](#keys-you-need-first) |
[Config](#settings-and-config) |
[Headless](#headless-and-agent-control) |
[Agent Workbench](#agent-workbench) |
[Status](#status) |
[Docs](#where-to-go-from-here)

</div>

![phux recording a terminal session and rendering it to a GIF, with no external tools](docs/assets/recording-demo.gif)

<div align="center">
<sub>

This GIF was recorded and rendered by the binary it is demonstrating.
No asciinema, no vhs, no agg, no ffmpeg -- `phux rec`, then `phux rec --from`.
Regenerate it with [`scripts/demo-record.sh`](scripts/demo-record.sh).

</sub></div>

phux is a terminal multiplexer: it keeps your shells running inside a
background server so you can split them into panes, disconnect, and come back
later with everything still running -- the job tmux and GNU screen do.

The difference is what a "terminal" is. In phux, every pane is a complete
terminal emulator living inside the server, and anything that speaks the phux
protocol can attach to it: the bundled TUI, a shell script, an AI agent, a
future GUI. All of them see and drive the same live terminal -- not a
screenshot of it, not text scraped out of it.

That one decision buys everything else in this README: detach and reattach
without losing modern terminal features, a typed command surface
(`snapshot`, `send-keys`, `wait`, `watch`) that works without a TTY, and
built-in recording. `phux --rec demo.gif` records while you work;
`phux rec <pane> -o demo.cast` observes a pane someone else is using without
attaching to it or resizing it; `phux play demo.cast` turns a recording back
into a live pane you can attach to, snapshot, or point an agent at. See
[Recording](docs/consumers/recording.md).

## How it works

One phux server runs per user. Your programs run as its children, on PTYs
the server owns -- that is why they survive when your terminal window
closes. For every PTY, the server runs a terminal emulator; every attached
client runs the same emulator again, one hop away:

```text
     your programs: zsh, vim, a build, an agent's shell
                          │
                          │  PTY
                          ▼
 ┌────────────────────────────────────────────────────────┐
 │ phux server -- one per user, keeps running after       │
 │ every window closes                                    │
 │                                                        │
 │   libghostty terminal: the authoritative one.          │
 │   Screen, scrollback, colors, modes, title -- the      │
 │   state of record lives here. It is what a client      │
 │   syncs from on reattach and what headless reads       │
 │   (snapshot, wait, watch) consult.                     │
 └──────────────────┬─────────────────────▲───────────────┘
                    │                     │
    terminal output │                     │ input flows up as
    flows down as   │                     │ structured key, mouse,
    raw VT bytes,   │                     │ focus, and paste
    exactly as the  │                     │ events; the server
    program wrote   │                     │ encodes them for
    them            ▼                     │ the PTY
 ┌────────────────────────────────────────┴───────────────┐
 │ phux client -- attach, detach, reattach at will;       │
 │ several clients can hold the same terminal at once     │
 │                                                        │
 │   libghostty terminal: the same engine again, fed      │
 │   the same bytes. It rebuilds the styled cell grid     │
 │   that the TUI draws on your screen.                   │
 └────────────────────────────────────────────────────────┘
```

**Why run libghostty on both sides?** A terminal emulator's job is turning a
stream of escape-coded bytes into a grid of styled cells. Multiplexers in
the tmux tradition do that job in the middle: they parse your program's
output into their own internal screen model, then re-encode that model as
fresh escape sequences for whatever terminal you attached from. Anything the
middle parser does not understand -- an inline image, a new underline style,
a protocol younger than the parser -- is degraded or dropped in translation.

phux never translates: it runs the same emulator -- [libghostty][lghv],
the terminal engine from Ghostty -- at both ends, with two different jobs:

- **The server-side terminal holds state.** It is the source of truth, and
  it exists so the terminal outlives any client: it is what a reattaching
  client syncs from and what scripts and agents read.
- **The client-side terminal renders.** It receives the exact bytes your
  program wrote and turns them into cells for the screen. Nothing is lost,
  because nothing was rewritten.

The wire between them is asymmetric on purpose. Downstream, terminal content
is raw VT bytes forwarded verbatim (a reattaching client first gets a
snapshot replayed from the server's state, then the live stream continues).
Upstream, input is structured key, mouse, focus, and paste events, encoded
into bytes only at the server -- because only the side that owns the
terminal knows which input protocols the running program has switched on.
The wire format is specified in [docs/spec/](./docs/spec/), and the
internals in [docs/architecture/](./docs/architecture/).

The visible result: Kitty graphics, truecolor, hyperlinks, OSC 133, and the
modern keyboard protocol survive detach and reattach, because nothing
between your program and your screen re-parses the bytes with a lesser
parser.

![a truecolor gradient, curly underlines, and an inline image surviving a detach and reattach, then the same session driven headlessly](docs/assets/demo.gif)

<div align="center">
<sub>

Screen-recorded in a graphics-capable terminal, deliberately: an inline image
is the one thing a cast cannot carry, so this clip is not re-renderable from
`phux rec` output. See
[Recording](docs/consumers/recording.md#2-what-a-recording-is-not).

</sub></div>

Two more consequences worth naming:

**Agents are first-class users.** An AI agent can drive the same terminal
you are looking at, over the wire, with the same authority you have. There
is no separate "agent mode" to enter. There are terminals, and some attached
users are people while others are programs.

**The terminal is the unit.** Sessions, windows, panes, and splits are TUI
arrangements around terminals. A script or agent can spawn a terminal, route
input to it, read its output, and wait for state changes without learning
the whole human UI model.

For the longer mental model, read [Concepts](./docs/CONCEPTS.md). For fit
and tradeoffs, read [When to use phux](./docs/when-to-use.md).

## Install and run

Fastest path on supported Homebrew platforms:

```sh
brew install phall1/phux/phux
phux
```

`phux` starts the server if needed and attaches a TUI client to the default
session. You are now inside a real shell running under phux. Detach with
`Ctrl-A d`; the server and your shell keep running. Run `phux` again to
reattach.

### Supported install channels

| Channel | Command |
|---|---|
| Homebrew (primary) | `brew install phall1/phux/phux` |
| Curl installer | `curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh \| bash` |
| Release tarball | download `phux-<tag>-<target>.tar.gz` plus `.sha256` from the [releases page](https://github.com/phall1/phux/releases) |
| From source | `nix develop -c cargo install --locked --path crates/phux` |

Releases ship prebuilt binaries for macOS arm64, Linux x86_64, and Linux arm64.
Every channel installs both `phux` and `phux-mcp`.

**Curl installer.** A wrapper around the GitHub release tarballs: it
verifies the release `.sha256` sidecar before unpacking and installs into
`${PHUX_INSTALL_DIR:-$HOME/.local/bin}` (make sure that is on your `PATH`).
With no `--version` it installs the latest release; pin one with
`bash -s -- --version vX.Y.Z`.

**From source.**

```sh
git clone https://github.com/phall1/phux
cd phux
nix develop -c cargo install --locked --path crates/phux
nix develop -c cargo install --locked --path crates/phux-mcp
```

The Nix dev shell pins Rust and the Zig compiler that libghostty's build
needs; the result is ordinary binaries in Cargo's bin directory. Off-Nix
pins and platform notes are in [INSTALL.md](./docs/INSTALL.md).

**The limits.** `cargo install phux` is unsupported: crates.io hosts only
`phux-protocol`, and the binary depends on a git-pinned `libghostty-vt`.
Windows is not supported. mise/asdf shims are not a supported install
channel yet.

### First run: persistent session + agent loop

Once `phux` is on `PATH`, start and attach:

```sh
phux
```

That auto-spawns a server and a shell-backed session. Detach with `Ctrl-A d`;
the shell keeps running. From another terminal, drive that same persistent
pane headlessly:

```sh
phux ls --json
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

Installing the bundled `phux-mcp` binary does not register it with an MCP
host. Follow [Registering with a host](./docs/consumers/mcp.md#registering-with-a-host)
for the Claude Code command, generic stdio configuration, and non-default
socket setup. The phux server must already be running before the host calls
a tool.

## Keys you need first

Inside phux, the default prefix is `Ctrl-A`:

| You want to | Press |
|---|---|
| Open the help overlay | `Ctrl-A ?` |
| Open the command palette | `Ctrl-A :` |
| Split side by side | `Ctrl-A %` |
| Split stacked | `Ctrl-A "` |
| Move between panes | `Ctrl-A h/j/k/l` |
| New tab/window | `Ctrl-A c` |
| Switch tab/window | `Ctrl-A n` / `Ctrl-A p` or `Ctrl-A 0`-`9` |
| Window/session picker | `Ctrl-A w` / `Ctrl-A s` |
| Rename window/session | `Ctrl-A ,` / `Ctrl-A $` |
| Copy mode | `Ctrl-A [` |
| Detach | `Ctrl-A d` |

## Settings and config

There is no settings modal. phux is config-file first: one TOML file
overlays the shipped defaults, and omitted keys keep following new defaults
from the binary. (Running from a source checkout before installing? Prefix
these with `cargo run --bin phux --`.)

| You want to | Run |
|---|---|
| See where config lives | `phux config path` |
| Create a commented starter config | `phux config init` |
| Print the effective merged config | `phux config show` |
| Print the shipped defaults with comments | `phux config show --default` |
| Validate configured plugins | `phux plugin validate` |
| Inspect plugins as JSON | `phux plugin list --json` |

Default config path:

```text
$XDG_CONFIG_HOME/phux/config.toml
# or, if XDG_CONFIG_HOME is unset:
~/.config/phux/config.toml
```

Edit the file, then restart the client to apply changes: detach and
reattach, or quit and run `phux` again. See
[Configuration and keybindings](./docs/CONFIG.md) for the schema, examples,
status widgets, hooks, and plugin manifests.

## Headless and agent control

Everything above also works without a TTY. The same terminals can be
addressed by name or id from scripts, CI, or an agent:

```sh
phux ls --json                         # list sessions and panes
phux snapshot .                        # read the focused pane
phux send-keys . 'cargo test' Enter    # type into the focused pane
phux run . "cargo test"                # run in a real pane, return its exit code
phux wait --until "0 failed" .         # block until output appears
phux watch --json .                    # stream pane events
```

Selectors are shared across the CLI:

| Selector | Meaning |
|---|---|
| `.` | current focused pane/window/session |
| `work` | session named `work` |
| `work:1.0` | session `work`, window 1, pane 0 |
| `@42` | opaque server-local terminal id |

One quirk: headless calls reject the `=` selector, because "previous pane"
only means something to an attached client with focus history. Inside the
TUI, `Ctrl-A =` jumps to the previous pane.

Register `phux-mcp` with the agent's host to expose the same core verbs over
JSON-RPC stdio, plus `phux_ask` and plugin workspace profile discovery.
Start with [Agents](./docs/consumers/agents.md) and
[MCP host registration](./docs/consumers/mcp.md#registering-with-a-host).

## Agent workbench

When agents run inside your terminals, you want to know what each one is
doing without attaching to it:

```sh
phux agent list --json
phux agent show . --json
phux agent explain .
phux ask . --id blocked-on-human --question "Which deploy target?"
```

`phux agent` reports each terminal's state, a confidence level, and whether
it needs attention; `explain` shows the evidence behind the verdict --
terminal identity, screen and title hints, plugin reports, and explicit
`ask` events -- instead of hiding a rule engine. `phux ask` lets an agent
park a question for a human to answer later.

The checked-in plugin package at
[`examples/plugins/agent-tools`](./examples/plugins/agent-tools/) provides
Codex and Claude Code integration records, lifecycle actions, and an
agent-bench workspace profile:

```sh
XDG_CONFIG_HOME="$PWD/examples/plugins/agent-tools/config" \
  phux config run com.phux.demo.agent-tools smoke-integrations
```

Those integrations are external and declarative. They can report
`missing`/`current`/`outdated`, link local session identity where available,
and run smoke checks without private credentials.

## Status

The line between shipped and promised is kept explicit:

**Stable enough to try**

- TUI attach, detach, reattach, multi-pane splits, status bar, keybindings,
  prefix-aware help hints, help overlay, and multiple clients on one session
- Modern-protocol passthrough: Kitty keyboard, truecolor, OSC 8, OSC 133,
  images
- Version-negotiated wire types in `phux-protocol`

**Real and tested, still pre-1.0**

- Headless verbs: `ls`, `snapshot`, `send-keys`, `run`, `wait`, `watch`,
  `ask`, `new`, `kill`, `rename`, `config`, `agent`, `plugin`, and
  `workspace` (`inspect`, `save`, `restore`)
- `phux-mcp`, exposing the same surface as MCP tools, including `phux_ask`
  and plugin workspace profile discovery
- Public Codex and Claude Code integration package fixtures with
  link/status/unlink/smoke actions
- Config scaffolding and effective-config inspection
- Workspace restore that recreates sessions and seed processes from a typed
  archive; live PTY handoff belongs to `phux upgrade`, not restore
- Predictive local echo behind the opt-in `[experimental]` configuration,
  with authoritative reconciliation and adaptive backoff
- Federation hubs that keep QUIC, WebSocket, or SSH-stdio satellite links
  connected, aggregate their terminal inventory, and route host-qualified
  commands; `phux satellite enroll HOST` bootstraps a box over SSH

**Designed and addressed-for, not wired yet**

- A native GUI consumer and a typed public Rust SDK crate.

Anything not in the first two lists is a direction, not a feature.

## Where to go from here

| You want to | Read |
|---|---|
| Run your first session | [Quickstart](./docs/QUICKSTART.md) |
| Install phux | [Install](./docs/INSTALL.md) |
| Customize keys and config | [Configuration](./docs/CONFIG.md) |
| Reach it from another network | [Remote access](./docs/remote-access.md) |
| Decide if phux fits | [When to use phux](./docs/when-to-use.md) |
| Understand the model | [Concepts](./docs/CONCEPTS.md) |
| Drive it from an agent | [Agents](./docs/consumers/agents.md) |
| Connect OpenCode | [OpenCode](./docs/consumers/opencode.md) |
| Connect Pi | [Pi](./docs/consumers/pi.md) |
| Use the MCP adapter | [MCP](./docs/consumers/mcp.md) |
| Read the wire spec | [Spec](./docs/spec/) |
| See how it is built | [Architecture](./docs/architecture/) |
| Ship a release | [Releasing](./docs/RELEASING.md) |
| Read where it is going | [Vision](./docs/vision.md) |
| See the decisions | [ADRs](./ADR/README.md) |
| Build it with us | [Contributing](./CONTRIBUTING.md) |

## Crates

| Crate | Does |
|---|---|
| `phux` | The binary: `attach` / `server` plus the headless verbs |
| `phux-protocol` | Wire types, codec, version negotiation; the crate meant for publishing |
| `phux-core` | Domain types: in-process terminal and collection registries |
| `phux-server` | The daemon: per-terminal actor, PTY supervision, output fanout |
| `phux-client-core` | Renderer and protocol client, ratatui-free |
| `phux-client` | The TUI chrome over `phux-client-core` |
| `phux-config` | TOML config schema and status widget contract |
| `phux-mcp` | The agent surface as MCP tools over JSON-RPC stdio |

## What phux deliberately will not do

Each of these is a "no" that keeps the model honest:

- **No embedded scripting language.** Commands are typed messages. Logic that
  wants a runtime can shell out to one.
- **No in-process plugin host.** Plugins are external packages declared in
  config and executed as argv; phux owns typed manifests, workspace state, and
  terminal control, not loaded plugin code.
- **No tmux-style copy-mode clone.** Selection formatting belongs to libghostty
  and native selection belongs to your terminal. phux owns focused-pane
  navigation and literal search over scrollback.
- **No homegrown crypto.** SSH and Unix-socket permissions are the trust model.
- **No format-template DSL.** The status bar takes typed widgets, not a printf
  dialect.

Full reasoning: [Contributing](./CONTRIBUTING.md).

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache-2.0](./LICENSE-APACHE).

[lghv]: https://github.com/Uzaaft/libghostty-rs
