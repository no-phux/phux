---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-09-30
---

# Quickstart

**TL;DR.** Start a shell in phux, detach, and return to the same running
terminal. Then use a second terminal to read and drive that pane without
changing the attached view. This walkthrough ends with a visible success
marker and a route to your coding agent or remote machine.

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

**Expected:** the installed binary prints its version. If the command is not
found, apply the installer's `PATH` advice before continuing. See
[installation recovery](./troubleshooting.md#phux-is-not-found-or-the-wrong-version-runs).

## 2. Start a terminal

```sh
phux
```

With no arguments, phux starts a per-user server if needed and attaches the
interactive client, creating a shell-backed session when needed. **Expected:**
you see a shell prompt inside phux. Type a harmless command such as `pwd` to
confirm the shell is ready.

The default prefix is `Ctrl-A`. **Press Ctrl and A together, release both,
then press the next key.** `Ctrl-A d` does not mean holding Ctrl while pressing d.

| Keys | Action |
|---|---|
| `Ctrl-A ?` | Open the complete keybinding help. |
| `Ctrl-A %` | Split left and right. |
| `Ctrl-A "` | Split top and bottom. |
| `Ctrl-A d` | Detach without stopping the shell. |

Try `Ctrl-A d`, then run `phux` again. **Expected:** you return to the same
live session, including the output you left on screen. Use [the TUI guide](./consumers/tui.md)
for more keys, copy mode, and navigation.

### What survives?

| Event | What to expect |
|---|---|
| Detach or close an attached client | The server keeps the shell and terminal state alive. |
| Planned live upgrade | The upgrade handoff preserves existing PTYs. |
| Server crash, server shutdown, or machine reboot | This is not live-process persistence; do not expect the old jobs or scrollback to return. |
| Restore a saved workspace | Fresh PTYs recreate the saved workspace, not the old processes. |

The exact boundaries and save/restore behavior live in
[workspace continuity](./operations.md#workspace-continuity-and-update-survival).
Save important work in the programs themselves; detaching is not a backup.

## 3. See it from the outside

Leave the interactive session running at an idle shell prompt and open a
second terminal. These commands address the focused pane with `.`:

```sh
phux ls
phux snapshot .
```

**Expected:** `ls` shows the session and `snapshot` shows its terminal text.
The snapshot does not attach or change the pane's size. If several panes are
active, choose an explicit [pane selector](./consumers/tui.md#selectors)
instead of `.` so you do not type into the wrong program.

Now send a command and wait for output that is not present in the typed
command itself:

```sh
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

**Expected:** the first terminal prints `PHUX-READY`, the wait succeeds, and
the JSON snapshot contains that output. This is the automation loop:

```text
read state -> act -> wait for a condition -> read again
```

A timeout is not proof that a command finished or failed. Read the pane again
and confirm you selected the idle shell before sending anything else. Avoid
human and agent input at the same time: both reach the same program.

## 4. Connect an agent

[Choose your coding-agent setup](./consumers/getting-started.md): Claude Code,
Pi, OpenCode, another MCP host, or a script. Each route starts with a small
read-only check before writing to a terminal. You do not need to implement an
AgentSession producer just to use an agent.

## Know the edges

Review [current limitations](./CONCEPTS.md#status) before relying on phux for
work that must survive a server failure.

## When something misbehaves

If no prompt appears, run `phux status` in another terminal. A missing server
and an unreachable remote host need different remedies; follow
[troubleshooting and recovery](./troubleshooting.md) rather than restarting
or deleting state as a first step.

## Next steps

| You want to | Go to |
|---|---|
| Change keys, status, or hooks | [Configuration](./CONFIG.md) |
| Run a coding agent | [Coding-agent getting started](./consumers/getting-started.md) |
| Automate with the full CLI contract | [Agent CLI reference guide](./consumers/agents.md) |
| Reach a server from another machine | [Remote access](./remote-access.md) |
| Understand sessions, windows, and panes | [How phux works](./CONCEPTS.md) |
| Compare tools and measured performance | [When to use phux](./when-to-use.md) |
