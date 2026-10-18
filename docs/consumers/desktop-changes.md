---
audience: humans, consumers, agents
stability: evolving
last-reviewed: 2026-10-18
---

# Desktop Changes panel

**TL;DR.** The desktop's Changes panel is a review surface for the focused
pane's Git checkout, beside the panes. It shows changed files as a tree and
every diff in one scroll, with changed words highlighted, syntax colours,
folded context and a side-by-side layout. You can stage whole files or just
selected lines, resolve conflicts, mark files viewed, and send line comments
to the agent's terminal. It follows focus and runs your own `git` on folders
on this Mac.

This page extends the [desktop contract](./desktop.md); the implementation
seam is in [desktop architecture](../architecture/desktop.md#changes-panel).

## Open it

**Toggle Changes** opens and closes the panel. Reach it with `Cmd-Alt-G`
(Super on Linux), the branch button in the titlebar, the sidebar view menu, or
the command palette. Drag its left edge to resize it; the panes always keep
320 points.

## What it shows

The panel reads the folder the focused terminal last reported and follows
focus and the shell's `cd`. Its header names the branch, the repository, and
how many commits are waiting to push or pull.

The file column groups files three ways:

| Group | Holds |
|---|---|
| Conflicts | Unmerged files. |
| Staged | Changes in the index, ready to commit. |
| Changes | Edits to tracked files, then new untracked files. |

Files sit under their folders, and a chain of single-folder parents shows as
one row, such as `clients/desktop/native`. Click a folder to fold it. The list
button in the header switches to a flat list. Each file shows its added and
removed line counts. A group lists at most 500 files and counts the rest.

The diff column shows one comparison at a time. **Changes** compares the
working tree with the index, and new files appear whole. **Staged** compares
the index with `HEAD`. Clicking a file switches to its comparison and scrolls
to it. Within a diff:

- Words that changed inside a line are emphasized over the line's tint.
- Syntax is coloured for common languages, such as Rust, TypeScript, Python,
  Go, Swift and Markdown.
- Unchanged runs fold to three lines of context, with controls to expand them.
- The layout button cycles **Automatic**, **Side by Side** and **Unified**.
  Automatic splits once the diff is about 820 points wide.
- The arrows jump to the previous or next change.

## Review

Each file header has a **Viewed** toggle. A viewed file folds, and the toolbar
counts how many of the comparison's files are viewed. The mark clears when the
file changes again, so a file the agent edits after you looked shows up again.

Hover a line, or select lines from the gutter, and click the add button to
write a note for the agent. **Enter** saves the note and **Shift+Enter** adds a
line. Notes stay with their lines. **Send Notes** pastes them, numbered and
labelled with file and line, into the terminal the panel follows. Nothing is
submitted: press **Enter** in that terminal to send the review.

## Stage and commit

- **Files:** a file header's **Stage** or **Unstage** button, or the button
  that shows when you hover a file row, moves the whole file. **Stage All** and
  **Unstage All** act on a whole group.
- **Lines:** select lines in the diff and click **Stage Lines** to stage only
  those added or removed lines. In the Staged comparison, **Unstage Lines**
  takes them back out of the index.
- **Conflicts:** a conflicted file shows its conflict regions with a choice of
  ours, theirs or both for each. Once every region has a choice, **Mark
  Resolved** writes the result and stages it.
- **Commit:** the message field commits the index. **Enter** commits and
  **Shift+Enter** adds a line. Commit is enabled only with something staged, no
  conflicts, and a message.

Hooks, config, signing, and ignore rules apply as in the terminal. A refused
commit posts git's own message and keeps your text.

## Limits

- Remote windows and terminals on another machine say why nothing is shown:
  their folders are not on this Mac.
- Reads never take the index lock, so an open panel's poll every few seconds
  does not contend with an agent's `git`. When the agent changes the shown
  comparison, the diff reloads and returns to the file you last opened.
- Very large comparisons are fetched with less surrounding context. New files
  over 1 MiB, binary files, and more than 200 new files are not previewed.
- Notes and viewed marks last until the window closes.
- The panel does not discard changes, push, pull, or switch branches.
