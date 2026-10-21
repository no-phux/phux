# Quick Capture

Quick Capture in the desktop app turns a thought into a new OpenCode
terminal with that thought as its first prompt. Use it when an idea comes up
mid-task and you want an agent to start on it without leaving what you are
doing.

Press <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd> in the app, type the
task, and press <kbd>Enter</kbd>.

## Opening and sending

Press <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd> while the app is in
front, or open the command palette (<kbd>Cmd</kbd>+<kbd>P</kbd>), type
<kbd>!</kbd>, and choose Capture. The shortcut works only inside the app; it
is not a system-wide hotkey.

| Key | Does |
| --- | --- |
| <kbd>Enter</kbd> | Sends, then opens the new terminal in a tab once it is ready. |
| <kbd>Cmd</kbd>+<kbd>Enter</kbd> (<kbd>Ctrl</kbd>+<kbd>Enter</kbd> on Linux) | Sends and stays in Capture, without opening a tab. |
| <kbd>Shift</kbd>+<kbd>Enter</kbd> | Adds a new line. |
| Dismiss | Closes Capture and keeps the draft. |

Capture keeps your draft, selection, and undo history while the app is
running, so closing and reopening it loses nothing. After a send succeeds,
the text you sent is cleared, unless you edited it while it was sending. A
refused send keeps the draft, and pressing send again while one is pending
does nothing.

A terminal sent with <kbd>Cmd</kbd>+<kbd>Enter</kbd> is an ordinary
interactive terminal. Find it in the sidebar or with Go To.

## Where it goes

Capture shows where the terminal will start before you send:

- Without a selector, it starts in the focused pane's session and folder.
- A leading `@session` starts it in that session, if the session has exactly
  one known folder on this machine.

A session it can't find, a session with more than one folder, and a folder
on another machine are refused. Capture never guesses another worktree and
never treats a remote folder as local.

Your text is passed to OpenCode as a single argument
(`opencode --prompt TEXT`), never through a shell.

## What a send means

Capture waits for the terminal to be ready, not for the agent. A successful
send doesn't mean OpenCode accepted the prompt: it can still stop to sign
in, to ask you to trust the folder, or on a startup error. Its permission
prompts work as usual. A send whose outcome is unclear is never retried
behind your back.

## Limits

- Drafts are not saved anywhere: quitting the app loses them.
- Only OpenCode is supported.
- Session names with spaces, repository or pane selectors, remote machines,
  creating a worktree, and sending to an agent that is already running are
  not supported.
