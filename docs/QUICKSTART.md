---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# Quickstart

**TL;DR.** Install phux, run `phux`, and you have a shell-backed terminal that
survives detach. Open a second terminal to inspect and drive that same pane
through the structured CLI. This guide gets both the human and agent paths
working before it sends you into configuration or protocol reference.

---

## 1. Install phux

On a Homebrew-supported macOS or Linux machine:

```sh
brew install phall1/tap/phux
```

This installs both `phux` and the bundled `phux-mcp` adapter. The
[`INSTALL.md`](./INSTALL.md) guide covers the verified curl installer, release
tarballs, supported platforms, and source builds.

Check that the binary is available:

```sh
phux --version
```

## 2. Start a terminal

```sh
phux
```

With no arguments, phux starts a per-user server if needed, creates a
shell-backed session, and attaches the interactive client. Work in it like a
normal terminal.

The default prefix is `Ctrl-A`. Four continuations are enough for a first run:

| Keys | Action |
|---|---|
| `Ctrl-A ?` | Open the complete keybinding help. |
| `Ctrl-A %` | Split left and right. |
| `Ctrl-A "` | Split top and bottom. |
| `Ctrl-A d` | Detach without stopping the shell. |

After detaching, run `phux` again. You return to the same live session.

## 3. See it from the outside

Leave the interactive session running and open a second terminal. The control
commands below address the focused pane with `.`:

```sh
phux ls
phux snapshot .
```

`ls` shows the sessions the server owns. `snapshot` reads the current terminal
without attaching to it or changing its size.

Now type into the same pane and wait for output that is not present in the
command itself:

```sh
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

That is the core automation loop:

```text
read state -> act -> wait for a condition -> read again
```

A script, coding agent, or MCP client uses this loop against the same terminal
a person can see and take over. Add `--json` to read commands when the caller
needs a versioned machine-readable result. Use `phux run` when you want phux to
execute a one-shot command and return its output and exit code directly.

## 4. Connect an agent

The release includes two agent-facing surfaces:

- The `phux` CLI for direct shell calls and scripts.
- `phux-mcp`, a JSON-RPC stdio adapter for MCP clients.

Start with the CLI guide for selectors, safe input, events, and result shapes:
[`consumers/agents.md`](./consumers/agents.md). Use
[`consumers/mcp.md`](./consumers/mcp.md) when the client speaks MCP.

Useful first commands:

```sh
phux ls --json
phux snapshot --json .
phux watch --json .
phux agent explain .
```

`watch` streams terminal events until interrupted. `agent explain` reports the
public state phux can infer for a coding agent in the pane, including its
confidence and evidence.

## Know the edges

phux is pre-alpha. Local persistent sessions, attach and detach, splits,
multiple clients, modern terminal passthrough, the headless CLI, and the MCP
adapter work today. Interfaces can still change before 1.0.

Hub-and-spoke federation now routes Terminal-scoped operations to configured
satellites; aggregate inventory exposes direct `host/@N` selectors, without
federated session/window joins. Predictive local
echo is implemented as an opt-in `[experimental]` setting and remains off by
default. The exact line between shipped behavior and design intent lives in
[`CONCEPTS.md`](./CONCEPTS.md); suitability by workflow lives in
[`when-to-use.md`](./when-to-use.md).

## When something misbehaves

Three commands are the starting point for any debugging session:

```sh
phux status
phux doctor
phux logs
```

`phux status` answers the first question -- is the server running -- in one
glance: its pid, how long it has been up, the protocol version it speaks,
attached clients, one line per session, and where its logs live. With no
server running it says so and names the commands that start one.
`phux doctor` checks the install end to end -- config validity, socket path,
server reachability, plugin manifests, and where the logs live -- and prints
one verdict per line, with a hint for anything that is not a pass. `phux logs`
names every log file phux writes (the canonical server log and the per-client
logs), says whether each exists yet, and tails them: `phux logs --server -f`
follows the server log, `phux logs --client` shows the newest client log.

## Next steps

| You want to | Go to |
|---|---|
| Change keys, status, or hooks | [`CONFIG.md`](./CONFIG.md) |
| Drive terminals from an agent | [`consumers/agents.md`](./consumers/agents.md) |
| Connect an MCP client | [`consumers/mcp.md`](./consumers/mcp.md) |
| Learn the terminal-on-a-wire model | [`CONCEPTS.md`](./CONCEPTS.md) |
| Study the internal Rust client | [`consumers/sdk.md`](./consumers/sdk.md) |
| Implement the protocol | [`spec/TUTORIAL.md`](./spec/TUTORIAL.md) |
| Build phux from source | [`../CONTRIBUTING.md`](../CONTRIBUTING.md) |
