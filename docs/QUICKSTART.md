---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-09-30
---

# Quickstart

**TL;DR.** Install phux, start a shell, detach, and reattach. Then read its
state, send a command, and wait for the result from a second terminal.

## Before you start

You need a supported macOS or Linux host, a terminal with interactive stdin
and stdout, and a shell. No account or config file is required. Use an idle
shell for the commands below, not an editor, REPL, or running agent.

## 1. Install phux

On Apple-silicon macOS or a supported Linuxbrew host:

```sh
brew trust --tap no-phux/tap # Homebrew 6+ only
brew tap no-phux/tap
brew install no-phux/tap/phux
```

This installs `phux` and the bundled `phux-mcp` adapter. Intel Macs need a
source build; Windows is not supported. For the platform matrix, curl
installer, tarballs, and source builds, use the [installation guide](./INSTALL.md).

```sh
phux --version
```

The command prints the installed version. If it is not found, follow the
installer's `PATH` advice or [installation recovery](./troubleshooting.md#phux-is-not-found-or-the-wrong-version-runs).

## 2. Start a terminal

```sh
phux
```

phux starts a per-user server if needed and attaches the interactive client,
creating a shell-backed session when needed. At the prompt, type `pwd` to
confirm the shell is ready.

The default prefix is `Ctrl-A`: press Ctrl and A together, release both, then
press the next key. `Ctrl-A d` does not mean holding Ctrl while pressing d.

| Keys | Action |
|---|---|
| `Ctrl-A ?` | Open the complete keybinding help. |
| `Ctrl-A %` | Split left and right. |
| `Ctrl-A "` | Split top and bottom. |
| `Ctrl-A d` | Detach without stopping the shell. |

Try `Ctrl-A d`, then run `phux` again. You return to the same live session,
including its screen output. See [the TUI guide](./consumers/tui.md) for more
keys, copy mode, and navigation.

### What survives?

| Event | What to expect |
|---|---|
| Detach or close an attached client | The server keeps the shell and terminal state alive. |
| Planned live upgrade | The upgrade handoff preserves existing PTYs. |
| Server crash, server shutdown, or machine reboot | This is not live-process persistence; do not expect the old jobs or scrollback to return. |
| Restore a saved workspace | Fresh PTYs recreate the saved workspace, not the old processes. |

See [workspace continuity](./operations.md#workspace-continuity-and-update-survival)
for the exact boundaries and save/restore behavior. Save important work in
the programs themselves; detaching is not a backup.

## 3. See it from the outside

Leave the interactive session running at an idle shell prompt and open a
second terminal. These commands address the focused pane with `.`:

```sh
phux ls
phux snapshot .
```

`ls` shows the session; `snapshot` reads its terminal text without attaching or
resizing the pane. With several active panes, choose an explicit
[pane selector](./consumers/tui.md#selectors) instead of `.` to avoid typing
into the wrong program.

Send a command, then wait for output distinct from the typed command:

```sh
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

The first terminal prints `PHUX-READY`, the wait succeeds, and the JSON
snapshot contains that output. The automation loop is:

```text
read state -> act -> wait for a condition -> read again
```

A timeout is not proof that a command finished or failed. Read the pane again
and confirm you selected the idle shell before sending anything else. Avoid
human and agent input at the same time: both reach the same program.

## 4. Connect an agent

[Choose a coding-agent setup](./consumers/getting-started.md): Claude Code,
Pi, OMP, OpenCode, another MCP host, or a script. Each starts with a read-only check.
You do not need an AgentSession producer to use an agent.

## Know the edges

Review [current limitations](./CONCEPTS.md#status) before relying on phux for
work that must survive a server failure.

## When something misbehaves

If no prompt appears, run `phux status` in another terminal and follow
[troubleshooting and recovery](./troubleshooting.md). Do not restart or delete
state before distinguishing a missing server from an unreachable host.

## Next steps

| You want to | Go to |
|---|---|
| Change keys, status, or hooks | [Configuration](./CONFIG.md) |
| Automate with the full CLI contract | [Agent CLI reference guide](./consumers/agents.md) |
| Reach a server from another machine | [Remote access](./remote-access.md) |
| Understand sessions, windows, and panes | [How phux works](./CONCEPTS.md) |
| Compare tools and measured performance | [When to use phux](./when-to-use.md) |
