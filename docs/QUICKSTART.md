---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-09-12
---

# Quickstart

**TL;DR.** Install phux, run `phux`, and you have a shell-backed terminal that
survives detach. Open a second terminal to inspect and drive that same pane
through the structured CLI.

---

## 1. Install phux

On a Homebrew-supported macOS or Linux machine:

```sh
brew trust --tap no-phux/tap # Homebrew 6+
brew tap no-phux/tap
brew install no-phux/tap/phux
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
normal terminal. The sidebar's **Agents** list is the fleet inbox: a filled
dot means an agent is waiting on you; a half-filled ring means it is still
working.

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

The same pane is addressable from a script or an MCP client:

```sh
phux ls --json
phux snapshot --json .
```

Selectors, input, wait, watch, and agent sessions:
[`consumers/agents.md`](./consumers/agents.md).

## Know the edges

Gaps: [`CONCEPTS.md`](./CONCEPTS.md#status).

## When something misbehaves

`phux status`, `phux doctor`, and `phux logs`. Details in the
[README](../README.md#troubleshooting) and [operations](./operations.md).

## Next steps

| You want to | Go to |
|---|---|
| Change keys, status, or hooks | [`CONFIG.md`](./CONFIG.md) |
| Drive terminals from an agent | [`consumers/agents.md`](./consumers/agents.md) |
| Reach a server from another machine | [`remote-access.md`](./remote-access.md) |
| Learn the model | [`CONCEPTS.md`](./CONCEPTS.md) |
| Other install channels | [`INSTALL.md`](./INSTALL.md) |
