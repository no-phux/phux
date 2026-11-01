# phux Neovim

One Neovim runs in the background behind all your panes, so buffers, registers,
undo and language servers are shared. `:split` and `:vsplit` open real phux
panes where Neovim would put the window, and closing a pane closes its window.
Floats, the completion menu and statuslines are drawn inside the pane they
belong to.

This is [neorex](https://github.com/alexrlocke/neorex)'s idea, for phux. It is a
plugin, not part of phux: a separate program with its own Cargo workspace and
no phux crate among its dependencies. It uses the `phux` command, the pane's
`PHUX_*` environment and the pane's terminal, and nothing else.

## Install

Needs `nvim` and, to build, `cargo`. Tested on Neovim 0.12.

```sh
phux plugin install no-phux/phux/plugins/nvim --yes
phux config reload
```

Open **Neovim (panes)** from the palette, or run the binary in any pane, with
any Neovim arguments:

```sh
alias pvim=~/.local/share/phux/plugins/com.phux.nvim/target/release/phux-nvim
pvim some/file.txt
```

## How it works

`phux-nvim` starts a daemon outside its pane, then stays as that pane's helper.
The daemon runs `nvim --embed` with `ext_multigrid` and turns every window
external, so each has its own grid. A new window gets a pane from `phux spawn`,
running a helper that relays input and resizes to the daemon and frames back.
Each frame sends only the cells that changed. When a helper exits, phux closes
its pane; when a pane goes away, the daemon closes its window.

## Focus

phux's focus belongs to each attached client, and a program can't move it. So
`<C-w>l` moves Neovim, but not phux's focus:

- Keys keep arriving from the pane you were in, and they follow Neovim to its
  current window. The cursor shows where they go. phux's focus highlight stays
  behind.
- Typing in a different pane selects that pane's window, and so does a click.
- One gap: after `<C-w>l`, moving phux's focus straight back to the pane you
  left with a phux key sends your keys to the other window until you click or
  use `<C-w>` again.

## Limitations

- `<C-w>` resize, swap and move commands do nothing. Use phux's pane controls.
- Plugins that arrange windows (sidebars, layout managers) don't get the
  placement they expect.
- One tab page: Neovim tab pages aren't phux tabs.
- No external command line or messages: long messages still show "Press ENTER".
- Local panes only: the daemon and its panes share a machine.

## Test

```sh
cargo test
cargo build --release --locked
tests/e2e.sh [path/to/phux]        # headless, on a throwaway server
tests/attached.py [path/to/phux]   # through a real client in a pty
```

Neither test touches your running server: each starts its own on a temporary
socket. CI doesn't run them, because this crate is outside the phux workspace.
