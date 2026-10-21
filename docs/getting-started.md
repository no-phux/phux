# Getting started

This assumes `phux` is [installed](./install.md) and on your `PATH`.

## First session

```sh
phux
```

The first run starts a server in the background and attaches you to a new
session named after the current directory. Later runs attach to the session
you used last.

Every key binding starts with the prefix, `Ctrl-A`. Press it and wait a
moment to see what comes next. `Ctrl-A Space` opens the command palette,
which lists every action with its key.

| Keys                                   | Does                                   |
| -------------------------------------- | -------------------------------------- |
| `Ctrl-A %` or `Ctrl-A \|`              | split side by side                     |
| `Ctrl-A "` or `Ctrl-A -`               | split top and bottom                   |
| `Ctrl-A h` `j` `k` `l`                 | move to the pane left, down, up, right |
| `Ctrl-A H` `J` `K` `L`                 | resize the pane                        |
| `Ctrl-A z`                             | zoom the pane                          |
| `Ctrl-A x`                             | close the pane (no confirmation)       |
| `Ctrl-A c`                             | new tab                                |
| `Ctrl-A n`, `Ctrl-A p`, `Ctrl-A 0`-`9` | next, previous, numbered tab           |
| `Ctrl-A ,`                             | rename the tab                         |
| `Ctrl-A s`                             | switch session                         |
| `Ctrl-A C`                             | new session                            |
| `Ctrl-A [`                             | copy mode                              |
| `Ctrl-A b`                             | toggle the sidebar                     |
| `Ctrl-A d`                             | detach                                 |

## Detach and come back

`Ctrl-A d` leaves the UI. The shells, and whatever is running in them, keep
going in the server. Run `phux` again to pick up where you were, from this
terminal or any other.

```sh
phux ls               # sessions on this machine
phux new work         # create a session called "work" and attach
phux attach work      # attach to it later
phux kill work        # end it and everything in it
```

## Scrollback and copying

The mouse wheel scrolls a pane's history. `Ctrl-A [` enters copy mode:

- arrows move, Shift+arrows extend the selection, PageUp/PageDown scroll
- `/` and `?` search, `n` and `N` repeat
- `w`, `v`, `A` select the word, the line, everything; `]` selects the last
  command's output
- `Enter` copies to the system clipboard and leaves, `Esc` leaves

Copying goes through your terminal's OSC 52 support, so it works over SSH
too, provided the terminal allows it.

## From another shell

Everything the UI can see is also available as commands, with no UI
attached. Pane ids such as `@1` are printed by `phux spawn`, listed by
`phux ls --json`, and set as `$PHUX_TERMINAL_ID` inside each pane.

```sh
phux snapshot @1                         # print the pane's screen
phux send-keys @1 'make test' Enter      # type into it
phux wait --regex 'passed|failed' @1     # block until the text shows up
phux run @1 'git status'                 # run a command, get its output and exit code
```

[Driving terminals from scripts](./scripting.md) covers this properly.

## Coming from tmux

The keys mostly match tmux with `Ctrl-A` instead of `Ctrl-B`. The
differences worth knowing:

| tmux                           | phux                                                |
| ------------------------------ | --------------------------------------------------- |
| arrow keys move between panes  | `h` `j` `k` `l`                                     |
| `;` last pane                  | `=` (`;` is previous pane)                          |
| `&` kill window                | `X` (asks first)                                    |
| `x` asks before killing a pane | `x` kills right away                                |
| `(` `)` previous/next session  | `P` `N`; `)` is last session                        |
| `[` copy mode with vi keys     | arrows only; `Enter` copies to the system clipboard |
| `q` display pane numbers       | `q` jumps to the next pane that needs you           |
| `tmux source-file`             | `phux config reload`                                |
| `tmux capture-pane -p`         | `phux snapshot`                                     |
| `tmux kill-server`             | `phux kill --server`                                |

To keep `Ctrl-B`, set `prefix = "C-b"` under `[keybindings]` in your
[config](./config.md). For when tmux is still the better fit, see
[phux vs tmux, Herdr, and cmux](./compare.md).

## Next

- [Concepts](./concepts.md): sessions, panes, targets, and what survives a
  restart.
- [Configuration](./config.md): keys, themes, the status bar.
- [Remote machines](./remote.md): attach to a server on another host.
