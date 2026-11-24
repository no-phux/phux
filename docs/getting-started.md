---
group: start
order: 2
---

# Getting started

Your first ten minutes: start phux, run an agent, split panes, detach, and
come back. [Concepts](./concepts.md) defines the terms.

## Start

```sh
curl -fsSL https://phux.sh/install | sh   # or see Install
phux
```

`phux` starts a background server and attaches you to a session named after
the current directory. Later runs attach to the session you used last.
Closing the terminal ends nothing; the server keeps your programs running.

## The prefix

Every phux key starts with the prefix <kbd>C-a</kbd>: press it, let go, then
press the key. Keys without the prefix go to the program in the pane.

- Press <kbd>C-a</kbd> and wait: a popup lists every key you can press next.
- <kbd>C-a</kbd> <kbd>Space</kbd> opens the command palette: run any action
  or jump to any session or tab, and see each one's key.

| Keys                                          | Does                         |
| --------------------------------------------- | ---------------------------- |
| <kbd>%</kbd>, <kbd>"</kbd>                     | split right, split below     |
| <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd> | move between panes      |
| <kbd>z</kbd>                                  | zoom the pane                |
| <kbd>x</kbd>                                  | close the pane (asks first)  |
| <kbd>c</kbd>, <kbd>n</kbd>, <kbd>p</kbd>       | new, next, previous tab      |
| <kbd>C</kbd>, <kbd>s</kbd>                     | new session, pick a session  |
| <kbd>b</kbd>                                  | show or hide the sidebar     |
| <kbd>d</kbd>                                  | detach                       |

[Default key bindings](./reference/keys.md) lists them all. The mouse works
too: click to focus, drag dividers and tabs, right-click for a menu.

## Detach and come back

<kbd>C-a</kbd> <kbd>d</kbd> detaches. Run `phux` again, from any terminal,
to pick up where you left off. A dropped SSH connection is the same as
detaching.

```sh
phux ls             # list sessions
phux new work       # create "work" and attach
phux attach work    # attach to it later
phux kill work      # end it (asks first)
```

## Run an agent

phux does not install agents. In a pane, run `claude`, `codex`, `opencode`,
or `pi`. Then, from another shell:

```sh
phux agent list
```

It prints each agent's pane, kind, and state. The sidebar shows the same.
When one is waiting on you, <kbd>C-a</kbd> <kbd>q</kbd> jumps to it and
<kbd>C-a</kbd> <kbd>Q</kbd> takes you back. [Coding agents](./agents.md)
covers setup; [Using phux as an agent](./agent-quickstart.md) drives
sessions from code.

## Scroll and copy

The mouse wheel scrolls; dragging copies. <kbd>C-a</kbd> <kbd>[</kbd> enters
copy mode, with vi keys:

| Keys                                          | Does                                                  |
| --------------------------------------------- | ----------------------------------------------------- |
| <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd>, arrows | move; at the edge, scroll                |
| <kbd>w</kbd> <kbd>b</kbd> <kbd>e</kbd>, <kbd>0</kbd> <kbd>^</kbd> <kbd>$</kbd> | word and line motions |
| <kbd>g</kbd> <kbd>G</kbd>, <kbd>H</kbd> <kbd>M</kbd> <kbd>L</kbd> | oldest and newest line; top, middle, bottom of screen |
| <kbd>C-u</kbd> <kbd>C-d</kbd>, <kbd>C-b</kbd> <kbd>C-f</kbd> | half page, page                         |
| <kbd>v</kbd> <kbd>V</kbd> <kbd>C-v</kbd>, <kbd>Tab</kbd> | select by character, line, block; switch between them |
| <kbd>/</kbd> <kbd>?</kbd>, <kbd>n</kbd> <kbd>N</kbd> | search down, up; repeat                     |
| <kbd>y</kbd> or <kbd>Enter</kbd>              | copy the selection (or the word) and leave            |
| <kbd>Y</kbd>, <kbd>A</kbd>, <kbd>]</kbd>       | copy the line, everything, or the last command's output |
| <kbd>Esc</kbd>, <kbd>q</kbd>                   | drop the selection; leave                             |

Copies reach your system clipboard through OSC 52, so they work over SSH
when your terminal allows it. <kbd>C-a</kbd> <kbd>U</kbd> picks a link from
the pane without selecting it. [Key bindings](./keys.md#copy-mode-keys)
rebinds copy-mode keys.

## Coming from tmux

To bring your tmux (or zellij, Ghostty) bindings over, see
[Coming from another multiplexer](./config.md#coming-from-another-multiplexer).

Most keys match tmux, with <kbd>C-a</kbd> for <kbd>C-b</kbd>. The
differences:

| tmux                                   | phux                                                   |
| -------------------------------------- | ------------------------------------------------------ |
| prefix <kbd>C-b</kbd>                  | <kbd>C-a</kbd>; set `[keybindings] prefix = "C-b"` to keep yours |
| window                                 | tab; config actions still say `window` (`new-window`)  |
| arrows move between panes              | <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd>    |
| <kbd>;</kbd> last pane                 | <kbd>=</kbd>; <kbd>;</kbd> is the previous pane        |
| <kbd>&</kbd> close window              | <kbd>X</kbd> closes a tab                              |
| <kbd>(</kbd> <kbd>)</kbd> sessions     | <kbd>P</kbd> <kbd>N</kbd>; <kbd>)</kbd> is the last session |
| <kbd>q</kbd> pane numbers              | <kbd>i</kbd>; <kbd>q</kbd> jumps to the pane that needs you |
| `synchronize-panes`                    | <kbd>y</kbd> toggles it for the tab                    |
| `send-prefix`                          | <kbd>C-a</kbd> <kbd>C-a</kbd>                          |
| `source-file`                          | `phux config reload`                                   |
| `capture-pane -p`                      | `phux snapshot`                                        |
| `kill-server`                          | `phux kill --server`                                   |

## Next

[Concepts](./concepts.md), [scripting](./scripting.md),
[key bindings](./keys.md), [remote machines](./remote.md).
