# Getting started

This page walks through a first session: start phux, split and move between panes, open tabs and sessions, detach, and come back. It assumes `phux` is [installed](./install.md).

```sh
phux
```

Press <kbd>C-a</kbd> <kbd>d</kbd> (<kbd>Ctrl</kbd>+<kbd>a</kbd>, then <kbd>d</kbd>) to detach. Run `phux` again to come back to exactly where you were.

## Start phux

```sh
phux
```

The first run starts a server in the background and attaches you to a new session named after the current directory. Your shell runs in a pane inside it. Later runs attach to the session you used last.

**Closing the terminal doesn't end anything.** The server keeps your shells and the programs in them running until you end them. [Concepts](./concepts.md) explains the server and what survives what.

## The prefix

Every phux key starts with the prefix, <kbd>C-a</kbd> (<kbd>Ctrl</kbd>+<kbd>a</kbd>). Press it, let go, then press the key for what you want. <kbd>C-a</kbd> <kbd>%</kbd> means "press <kbd>C-a</kbd>, then <kbd>%</kbd>".

Keys you press without the prefix go to the program in the pane. phux binds none of them out of the box.

## Find any key

You don't need to memorize anything on this page. Two things show you the keys as you go:

- **The which-key popup.** Press <kbd>C-a</kbd> and wait a moment. A popup lists every key you can press next, grouped into Pane, Tab, Session, and phux. It never eats a key: press one and it runs as if the popup weren't there. <kbd>Esc</kbd> cancels the prefix.
- **The command palette.** Press <kbd>C-a</kbd> <kbd>Space</kbd> (or <kbd>:</kbd> or <kbd>?</kbd>) to search every action by name. Each row shows its key, so the palette is also how you learn them.

[Default key bindings](./reference/keys.md) lists every key on one page.

## Split and move between panes

A pane is one terminal. Split the one you're in:

| Keys                                                                               | Does                                     |
| ---------------------------------------------------------------------------------- | ---------------------------------------- |
| <kbd>C-a</kbd> <kbd>%</kbd> or <kbd>C-a</kbd> <kbd>\|</kbd>                        | split right                              |
| <kbd>C-a</kbd> <kbd>"</kbd> or <kbd>C-a</kbd> <kbd>-</kbd>                         | split below                              |
| <kbd>C-a</kbd> <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd>                 | move to the pane left, down, up, right   |
| <kbd>C-a</kbd> <kbd>H</kbd> <kbd>J</kbd> <kbd>K</kbd> <kbd>L</kbd>                 | resize the pane left, down, up, right    |
| <kbd>C-a</kbd> <kbd>R</kbd>                                                        | resize mode: <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd> or the arrows resize one cell at a time; <kbd>Esc</kbd> leaves |
| <kbd>C-a</kbd> <kbd>o</kbd>                                                        | next pane                                |
| <kbd>C-a</kbd> <kbd>=</kbd>                                                        | the pane you were in before              |
| <kbd>C-a</kbd> <kbd>z</kbd>                                                        | zoom the pane to fill the tab; again to unzoom |
| <kbd>C-a</kbd> <kbd>x</kbd>                                                        | close the pane                           |

**<kbd>C-a</kbd> <kbd>x</kbd> closes the pane right away, without asking**, and ends the program in it. Capital letters mean <kbd>Shift</kbd>: <kbd>H</kbd> is <kbd>Shift</kbd>+<kbd>h</kbd>.

## Tabs

A tab holds a set of panes. The status bar along the top shows your tabs.

| Keys                                                       | Does                                       |
| ---------------------------------------------------------- | ------------------------------------------ |
| <kbd>C-a</kbd> <kbd>c</kbd>                                | new tab                                    |
| <kbd>C-a</kbd> <kbd>n</kbd> or <kbd>C-a</kbd> <kbd>Tab</kbd>, <kbd>C-a</kbd> <kbd>p</kbd> | next, previous tab |
| <kbd>C-a</kbd> <kbd>0</kbd>–<kbd>9</kbd>                   | go to tab 0–9                              |
| <kbd>C-a</kbd> <kbd>w</kbd>                                | pick a tab from every session              |
| <kbd>C-a</kbd> <kbd>,</kbd>                                | rename the tab                             |
| <kbd>C-a</kbd> <kbd>X</kbd>                                | close the tab and everything in it (asks first) |

## Sessions

A session is a named workspace, usually one per project. Each holds its own tabs.

| Keys                                                       | Does                         |
| ---------------------------------------------------------- | ---------------------------- |
| <kbd>C-a</kbd> <kbd>C</kbd>                                | new session                  |
| <kbd>C-a</kbd> <kbd>s</kbd>                                | pick a session               |
| <kbd>C-a</kbd> <kbd>N</kbd>, <kbd>C-a</kbd> <kbd>P</kbd>   | next, previous session       |
| <kbd>C-a</kbd> <kbd>)</kbd>                                | the session you were in before |
| <kbd>C-a</kbd> <kbd>$</kbd>                                | rename the session           |

From a shell:

```sh
phux ls               # list sessions
phux new work         # create a session called "work" and attach
phux attach work      # attach to it later
phux kill work        # end it and everything in it (asks first)
```

## Detach and come back

Press <kbd>C-a</kbd> <kbd>d</kbd> to detach. The UI closes; your shells, and whatever runs in them, keep going.

```sh
phux
```

Run `phux` again, from this terminal or any other, to pick up where you left off. Losing an SSH connection is the same as detaching.

## Scroll and copy

Scroll a pane's history with the mouse wheel. For keyboard selection, press <kbd>C-a</kbd> <kbd>[</kbd> to enter copy mode:

| Keys                                          | Does                                                    |
| --------------------------------------------- | ------------------------------------------------------- |
| arrow keys                                    | move the cursor; at the edge, scroll                    |
| <kbd>Shift</kbd>+arrows                       | extend the selection                                    |
| <kbd>PageUp</kbd>, <kbd>PageDown</kbd>        | scroll a page                                           |
| <kbd>Tab</kbd>                                | switch between character, line, and block selection     |
| <kbd>/</kbd>, <kbd>?</kbd>                    | search down, up; <kbd>n</kbd> and <kbd>N</kbd> repeat   |
| <kbd>Enter</kbd>                              | copy the selection and leave                            |
| <kbd>w</kbd>, <kbd>v</kbd>, <kbd>A</kbd>      | copy the word, the line, or everything, and leave       |
| <kbd>]</kbd>                                  | copy the last command's output and leave                |
| <kbd>Esc</kbd>                                | leave without copying                                   |

Copies go to your system clipboard through your terminal (OSC 52), so they work over SSH too if your terminal allows it.

## Mouse

The mouse is on by default:

- Click a pane to focus it.
- Drag a divider to resize panes.
- Drag across text to copy it.
- Right-click for a menu of actions for that pane, tab, or session.
- Click a tab to switch to it; drag it to reorder.

Programs that use the mouse themselves, such as `htop` or an editor, still get clicks over their own panes. To leave the mouse to your terminal's own selection instead, set `mouse = false` under `[defaults]` in your [config file](./config.md).

## The sidebar

The sidebar lists your coding agents (with which ones need you) and your sessions. Press <kbd>C-a</kbd> <kbd>b</kbd> to show or hide it. On a narrow terminal it stays hidden so your panes get the room.

When an agent is waiting on you, <kbd>C-a</kbd> <kbd>q</kbd> jumps to it and <kbd>C-a</kbd> <kbd>Q</kbd> takes you back. <kbd>C-a</kbd> <kbd>A</kbd> opens every agent and its state. [Coding agents](./agents.md) covers the rest.

## Coming from tmux

Most keys match tmux, with <kbd>C-a</kbd> where tmux uses <kbd>C-b</kbd>. What's different:

| In tmux                                     | In phux                                                                |
| ------------------------------------------- | ---------------------------------------------------------------------- |
| prefix <kbd>C-b</kbd>                       | prefix <kbd>C-a</kbd>; [set it back](#keep-c-b) if you like            |
| "window"                                    | "tab" in the UI; actions in the config file still say `window` (`new-window`) |
| arrow keys move between panes               | <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd>                    |
| <kbd>;</kbd> last pane                      | <kbd>=</kbd>; <kbd>;</kbd> is the previous pane                        |
| <kbd>x</kbd> asks before closing a pane     | <kbd>x</kbd> closes it right away                                      |
| <kbd>&</kbd> closes a window                | <kbd>X</kbd> closes a tab (asks first)                                 |
| <kbd>(</kbd> <kbd>)</kbd> previous, next session | <kbd>P</kbd> <kbd>N</kbd>; <kbd>)</kbd> is the last session         |
| <kbd>q</kbd> shows pane numbers             | <kbd>q</kbd> jumps to the next pane that needs you                     |
| `send-prefix` sends the prefix through      | press the prefix twice: <kbd>C-a</kbd> <kbd>C-a</kbd> sends <kbd>C-a</kbd> to the program. Any key with no binding after the prefix goes to the program |
| copy mode with vi or emacs keys             | arrow keys; <kbd>Enter</kbd> copies to the system clipboard            |
| `tmux source-file ~/.tmux.conf`             | `phux config reload`                                                   |
| `tmux capture-pane -p`                      | `phux snapshot`                                                        |
| `tmux kill-server`                          | `phux kill --server`                                                   |

For when tmux is still the better fit, see [phux vs tmux, Herdr, and cmux](./compare.md).

### Keep C-b

Put this in `~/.config/phux/config.toml`:

```toml
[keybindings]
prefix = "C-b"
```

Then reload with `phux config reload`. [Key bindings](./keys.md) covers rebinding and unbinding any key.

## Next

- [Concepts](./concepts.md): sessions, tabs, panes, targets, and what survives a restart.
- [Driving terminals from scripts](./scripting.md): everything above, from a shell or a coding agent.
- [Key bindings](./keys.md): make the keys your own.
- [Remote machines](./remote.md): attach to a server on another machine.
