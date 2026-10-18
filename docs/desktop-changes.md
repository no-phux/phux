---
audience: humans, consumers, agents
stability: evolving
last-reviewed: 2026-10-18
---

# Desktop Changes panel

**TL;DR.** The desktop's Changes panel shows the focused pane's Git
checkout beside the panes: branch, push/pull counts, changed files, diffs,
staging and commit. It follows focus, so an agent's edits show next to the
pane it runs in. It runs your own `git` and works only on folders on this machine.

See the [desktop overview](./clients.md#desktop) for installation and sessions.
The [implementation guarantees](#implementation-guarantees) below explain the
Git and UI seams.

## Open it

**Toggle Changes** opens and closes the panel. Reach it with `Cmd-Alt-G`
(Super on Linux), the branch button in the titlebar, the sidebar view menu, or
the command palette.

## What it shows

The panel reads the folder the focused terminal last reported and follows
focus and the shell's `cd`. Its header names the branch, the repository, and
how many commits are waiting to push or pull. Files follow in three groups:

| Group | Holds |
|---|---|
| Conflicts | Unmerged files. |
| Staged | Changes in the index, ready to commit. |
| Changes | Edits to tracked files, then new untracked files. |

Click a file to open its diff under the row, with line numbers and added and
removed lines washed in the theme's success and danger colours. Click it again
to close it. A section lists at most 500 files and counts the rest.

## Stage and commit

Hover a row to stage it, unstage it, or mark a conflict resolved. **Stage All**
and **Unstage All** act on a whole group. The message field commits the index:
**Enter** commits and **Shift+Enter** adds a line. Commit is enabled only with
something staged, no conflicts, and a message.

Hooks, config, signing, and ignore rules apply as in the terminal. A refused
commit posts git's own message and keeps your text.

## Limits

- Remote windows and terminals on another machine say why nothing is shown:
  their folders are not on this Mac.
- Status reads never take the index lock, so an open panel's poll every few
  seconds does not contend with an agent's `git`.
- The panel does not discard changes, push, pull, or switch branches.

## Implementation guarantees

`clients/desktop/native/src/git.rs` owns Git operations without a GPUI
dependency. It invokes the user's `git`, parses porcelain-v2 branch/status
records and unified diffs, and applies stage, unstage, and commit. Read-side
commands set `GIT_OPTIONAL_LOCKS=0`; commands never prompt on the UI thread
(`GIT_TERMINAL_PROMPT=0`). Hooks, configuration, signing, credential helpers,
and ignore rules therefore follow the terminal rather than a second Git
implementation.

`clients/desktop/native/src/app/changes.rs` binds the panel to the focused
placement's reported `cwd`. Remote windows and satellite terminals are
refused, not interpreted as local paths. Calls run through `smol::unblock`;
a generation counter rejects results after the panel moves to another folder.
Status polling notifies the desk only when status or an open diff changes.
`app/view/changes.rs` is cached chrome, so terminal frames do not rebuild the
file list or diff.
