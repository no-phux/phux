# Chief of staff

In the desktop app on macOS, a tap of the right <kbd>⌘</kbd> key on its
own opens a composer for one agent, the chief of staff. What you type or
dictate goes to that agent as one typed [ACP](./acp.md) prompt. It is
never typed into a terminal, and no screen is read.

## Start the chief

The chief is the ACP agent in the phux session named `chief`. Create the
session, then launch an agent that speaks ACP into it:

```sh
phux new --json -s chief
phux launch opencode --acp --session chief
```

Any ACP agent phux can launch works; `phux launch --list --acp` shows them.
An agent running in a plain pane cannot be the chief, because the composer
never types into a terminal.

## Talk to it

| Key | Does |
| --- | --- |
| right <kbd>⌘</kbd>, tapped alone | Opens the composer, focused, with any draft you left. |
| <kbd>Return</kbd> | Sends the draft as one prompt. |
| <kbd>⇧ Return</kbd> | Breaks the line. |
| <kbd>Esc</kbd> | Puts the composer away and keeps the draft. |

The composer opens when the key comes back up, however long you held it. A
dictation app that records while you hold the key, such as Hex, pastes what
it heard into the composer as you let go. Holding the key with another key,
a click, or another modifier is a shortcut, not a tap. The tap works only
while a terminal has the keyboard, so it never takes a chord from a field
or sheet.

## What a send means

ACP reports a prompt as queued, not as read, so the composer says
**Queued** and clears the draft. An agent still in a turn refuses a new
prompt. A refusal, or a send whose outcome is unknown, keeps the draft and
says why. Nothing is ever resent; read the agent before you send again.

If the session is missing, runs no ACP agent, or the agent's controller is
disconnected, nothing is sent and the composer says which.

## What the chief can reach

phux delegates an ACP agent's access to its own session. A chief can list
and prompt the agents in the `chief` session, not agents in other
sessions.

## Configure

`[rune] chief` in [config.toml](./config.md#rune) names the session:

```toml
[rune]
chief = "staff"   # "chief" when unset; "none" turns the tap off
```
