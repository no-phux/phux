---
audience: humans, contributors
stability: stable
last-reviewed: 2026-09-27
---

# Coming from tmux or screen

**TL;DR.** Translate existing multiplexer muscle memory into phux. tmux users
keep attach, split, and prefix habits; screen users keep detach and reattach.
phux is not a drop-in configuration replacement: its extra surface is a live
resource wire used by visual clients, scripts, and agents.

---

Install and the first attach are in [Quickstart](./QUICKSTART.md). This
page is the translation layer.

## tmux

The default prefix is `Ctrl-A`, not `Ctrl-B`. Continuations that exist in
both tools do the same job:

| tmux | phux | |
|---|---|---|
| `prefix %` | `Ctrl-A %` | Split left and right |
| `prefix "` | `Ctrl-A "` | Split top and bottom |
| `prefix d` | `Ctrl-A d` | Detach; the shell keeps running |
| `prefix c` | `Ctrl-A c` | New window |
| `tmux a` | `phux` | Reattach |

`Ctrl-A ?` is the complete keybinding list. Config lives in
`~/.config/phux/config.toml`; [Configuration](./CONFIG.md) is the
reference.

phux is not a tmux clone: there is no scripting language, plugin host, or
server-side copy-mode, and every pane is a real terminal that a TUI, Cockpit,
the CLI, or an agent attaches to as a peer.
[When to use phux](./when-to-use.md) weighs that against tmux.

## If you used the old phux starter

An old in-tree configuration bundle was briefly named `herdr`; it has no
relationship to [Herdr](https://herdr.dev). Its opinions are now the shipped
defaults, and its remaining demo plugin set is the `starter` distro
(`phux config init --distro starter`; `--distro herdr` is an alias). See
[Configuration](./CONFIG.md#starter-distributions-config-init---distro).

## screen

`phux` starts a server if needed and attaches. `Ctrl-A d` detaches.
`phux` brings you back. Named sessions, splits, and a status bar are in
the TUI; remote attach is [Remote access](./remote-access.md), not
`screen -x` over SSH.

## Next

- [Quickstart](./QUICKSTART.md)
- [Concepts](./CONCEPTS.md)
- [Agents](./consumers/agents.md)
- [Cockpit](./INSTALL.md#cockpit-native-macos)
