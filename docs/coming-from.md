---
audience: humans, contributors
stability: stable
last-reviewed: 2026-09-27
---

# Coming from tmux or screen

**TL;DR.** Keep the attach, split, and detach workflow; use `Ctrl-A` as the
prefix. phux uses TOML configuration, not tmux or screen config files.
Scripts and agents can control terminals through its public resource protocol.

---

For installation and first attach, use the [quickstart](./QUICKSTART.md).

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

phux has no tmux scripting language, in-process plugin host, or server-side
copy mode. The TUI, Cockpit, CLI, and agents use the same terminal protocol.
[When to use phux](./when-to-use.md) weighs that against tmux.

## If you used the old phux starter

The old in-tree `herdr` configuration bundle is unrelated to
[Herdr](https://herdr.dev). Its settings are now the shipped defaults;
its demo plugins remain in the `starter` distro
(`phux config init --distro starter`; `--distro herdr` is an alias). See
[Configuration](./CONFIG.md#starter-distributions-config-init---distro).

## screen

Run `phux` to start or reattach; `Ctrl-A d` detaches. The TUI has named
sessions, splits, and a status bar. For another machine, use
[remote access](./remote-access.md), not `screen -x` over SSH.

## Next

- [Concepts](./CONCEPTS.md)
- [Agents](./consumers/agents.md)
- [Cockpit](./INSTALL.md#cockpit-native-macos)
