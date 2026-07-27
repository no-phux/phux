<!--
audience: humans, contributors, agents
stability: stable
last-reviewed: 2026-06-23
-->

<div align="center">

<img src="docs/assets/logo.svg" alt="phux" width="420">

# phux

**the tmux job, done - a terminal is an object on a wire**

[![CI](https://github.com/phall1/phux/actions/workflows/ci.yml/badge.svg)](https://github.com/phall1/phux/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

[Install](#install-and-run) |
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

phux is a terminal multiplexer: attach, split panes, detach, and come back
later to the same shells. The difference is underneath. In phux, a terminal is
a first-class object on a wire, so your TUI, a GUI, and an AI agent can all
hold the same live terminal instead of reading screenshots or copied text.

Because a terminal is an object rather than a screenshot, phux can hand you a
recording of one: `phux --rec demo.gif` while you work, or `phux rec <pane>
-o demo.cast` against a pane someone else is using -- a pure observer that
never attaches and never resizes what it is watching. And because a terminal
is an object, `phux play demo.cast` gives the recording back as one: a pane
whose PTY is fed from the cast, which you can attach to, snapshot, resize, or
point an agent at. See [Recording](docs/consumers/recording.md).

If you are new here, install phux, run it, detach, reattach, and drive the same
pane from a script without hunting through the docs.

## Install and run

Fastest path on supported Homebrew platforms:

```sh
brew install phall1/phux/phux
phux
```

Portable installer for the latest GitHub release:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash
export PATH="$HOME/.local/bin:$PATH"
phux
```

Install from a source checkout:

```sh
git clone https://github.com/phall1/phux
cd phux
nix develop -c cargo install --locked --path crates/phux
nix develop -c cargo install --locked --path crates/phux-mcp
phux
```

`phux` starts the server if needed and attaches a TUI client to the default
session. You are now inside a real shell running under phux. Detach with
`Ctrl-A d`; the server and pane processes keep running. Reattach with:

```sh
phux
```

No Nix? Use [Install](./docs/INSTALL.md) for the current release channels and
off-Nix build notes. The GitHub latest release is `v0.0.3`, with `phux` and
`phux-mcp` artifacts for macOS arm64, Linux x86_64, and Linux arm64.

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

After detaching, the server and pane processes keep running. Reattach with
`phux`.

## Settings and config

There is no settings modal. phux is config-file first: one TOML file overlays
the shipped defaults, and omitted keys keep following new defaults from the
binary.

If you are running from a source checkout before installing the binary, prefix
these commands with `cargo run --bin phux --`, for example
`cargo run --bin phux -- config path`.

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

Edit the file, then restart the client to apply changes: detach and reattach,
or quit and run `phux` again. See [Configuration and keybindings](./docs/CONFIG.md)
for the schema, examples, status widgets, hooks, and plugin manifests.

## Install paths

phux is v0.0.x, but the public install surface now exists: Homebrew, the curl
installer, release tarballs, and source installs from a clone all install both
`phux` and `phux-mcp`.

### Supported install channels

| Channel | Status | Command |
|---|---|---|
| Homebrew | Primary binary path on supported Homebrew platforms | `brew install phall1/phux/phux` |
| Curl installer | Scripted install from GitHub release tarballs | `curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh \| bash` |
| Release tarball | Manual install and checksum verification | download `phux-<tag>-<target>.tar.gz` plus `.sha256` |
| From source | Source install from a clone on macOS and Linux | `nix develop -c cargo install --locked --path crates/phux` |

Homebrew is the cleanest install path when your platform has a published
Formula artifact:

```sh
brew install phall1/phux/phux
```

**Curl installer.**

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash
```

The installer is a wrapper around GitHub release tarballs. It verifies the
release `.sha256` sidecar before unpacking and installs both `phux` and
`phux-mcp` into `${PHUX_INSTALL_DIR:-$HOME/.local/bin}`; set
`PHUX_INSTALL_DIR` to choose a different bin directory. With no `--version`, it
installs the latest GitHub release.

To pin a specific release:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash -s -- --version v0.0.3
```

**Prebuilt release artifacts.** Version tags build tarballs for macOS arm64,
Linux x86_64, and Linux arm64. `v0.0.2` is the first portable public release;
the seeded `v0.0.1` Linux tarball was Nix-linked and should be ignored.

**From source.**

```sh
git clone https://github.com/phall1/phux
cd phux
nix develop -c cargo install --locked --path crates/phux
nix develop -c cargo install --locked --path crates/phux-mcp
phux
```

The Nix dev shell pins Rust and the Zig compiler required by libghostty; the
commands above still install normal `phux` and `phux-mcp` binaries into
Cargo's bin directory. Off-Nix pins and platform notes are in
[INSTALL.md](./docs/INSTALL.md).

`cargo install phux is unsupported`: crates.io is scoped to `phux-protocol`;
the binary and internal crates are not publishable, and the binary still
depends on a git-pinned `libghostty-vt`. Windows is not supported. mise/asdf
shims are not a supported install channel yet.

### First run: persistent session + agent loop

Once `phux` is on `PATH`, start and attach:

```sh
phux
```

That auto-spawns a server and a shell-backed session. Detach with `Ctrl-A d`;
the shell keeps running. In another terminal, exercise the agent loop against
that same persistent pane:

```sh
phux ls --json
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

Installing the bundled `phux-mcp` binary does not register it with an MCP
host. Follow [Registering with a host](./docs/consumers/mcp.md#registering-with-a-host)
for the Claude Code command, generic stdio configuration, and non-default
socket setup. The phux server must already be running before the host calls a
tool.

## Headless and agent control

Everything above also works without a TTY. The same terminals can be addressed
by name or id from scripts, CI, or an agent:

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

Headless CLI/MCP calls reject `=` because they have no attached-client focus
history. In the attached TUI, `C-a =` jumps to the previous pane. This local
MRU behavior depends on the focus model recorded by ADR-0049 on the sibling
focus branch; this branch does not duplicate that ADR.

Register `phux-mcp` with the agent's host to expose the same core verbs over
JSON-RPC stdio, plus `phux_ask` and plugin workspace profile discovery. Start
with [Agents](./docs/consumers/agents.md) and
[MCP host registration](./docs/consumers/mcp.md#registering-with-a-host).

## Agent workbench

phux now has the public pieces that make an agent bench feel first-class
without copying another app's plugin host:

```sh
phux agent list --json
phux agent show . --json
phux agent explain .
phux ask . --id blocked-on-human --question "Which deploy target?"
```

`phux agent` is an explainable projection over phux-owned evidence: terminal
identity, screen/title hints, plugin reports, and explicit `ask` events. It
returns state, confidence, attention, and source provenance instead of hiding a
rule engine.

The checked-in plugin package at
[`examples/plugins/agent-tools`](./examples/plugins/agent-tools/) provides
public Codex and Claude Code integration records, lifecycle actions, and an
agent-bench workspace profile:

```sh
XDG_CONFIG_HOME="$PWD/examples/plugins/agent-tools/config" \
  phux config run com.phux.demo.agent-tools smoke-integrations
```

Those integrations are external and declarative. They can report
`missing`/`current`/`outdated`, link local session identity where available,
and run smoke checks without private credentials.

## Why it is different

**Modern terminals stay modern across a reattach.** Kitty graphics, truecolor,
hyperlinks, OSC 133, and the modern keyboard protocol survive detach/reattach
because phux does not re-parse your bytes in the middle. The same terminal
engine ([libghostty][lghv]) runs on both ends of the wire.

![a truecolor gradient, curly underlines, and an inline image surviving a detach and reattach, then the same session driven headlessly](docs/assets/demo.gif)

<div align="center">
<sub>

Screen-recorded in a graphics-capable terminal, deliberately: an inline image
is the one thing a cast cannot carry, so this clip is not re-renderable from
`phux rec` output. See
[Recording](docs/consumers/recording.md#2-what-a-recording-is-not).

</sub></div>

**Agents are first-class users.** An AI agent can drive the same terminal you
are looking at, over the wire, with the same authority you have. There is no
separate "agent mode" to enter. There are terminals, and some attached users
are people while others are programs.

**The terminal is the unit.** Sessions, windows, panes, and splits are TUI
arrangements around terminals. A script or agent can spawn a terminal, route
input to it, read its output, and wait for state changes without learning the
whole human UI model.

For the longer mental model, read [Concepts](./docs/CONCEPTS.md). For fit and
tradeoffs, read [When to use phux](./docs/when-to-use.md).

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
- `phux-mcp`, exposing the same surface as MCP tools, including `phux_ask` and
  plugin workspace profile discovery
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
