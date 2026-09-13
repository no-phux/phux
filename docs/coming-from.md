---
audience: humans, contributors
stability: stable
last-reviewed: 2026-09-13
---

# Coming from tmux, screen, or the old phux distro

**TL;DR.** Translate existing multiplexer muscle memory into phux. tmux
users keep attach, split, and prefix habits. Anyone who used the in-tree
starter formerly named `herdr` already has those defaults as stock phux.
screen users get detach without the rest of screen. phux's extra surface
is that every pane is a live terminal other programs attach to.

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

What is not a tmux clone:

- There is no tmux scripting language, plugin host, or copy-mode
  reimplementation on the server. Copy/navigation is a client overlay.
- Each pane is a real terminal emulator in the server, so a second
  client — TUI, Cockpit, CLI, agent — attaches to the same live
  terminal instead of parsing a byte stream in the middle.
- Headless control is `phux ls`, `phux snapshot`, `phux send-keys`,
  `phux wait`, plus `phux-mcp`. That is the agent surface, not a
  socket command dialect.

If you want a battle-hardened local multiplexer and nothing else, tmux
is still the answer. phux is the multiplexer you use when a human and
an agent should share the same terminal. [When to use phux](./when-to-use.md).

## The old phux `herdr` distro

This section is the in-tree starter formerly named `herdr`, not the
separately developed herdr.dev product.

That starter's opinions — which-key delay, split and palette chords, tab
strip, tokyonight chrome — are the shipped defaults. A naked `phux`
already behaves the way installing that distro used to.

What remains as a distro is the demo plugin set (workspace
autosave/restore and agent-tools), because an embedded default cannot
carry relative plugin paths. That layer is now named `starter`:

```sh
phux config init --distro starter
```

`--distro herdr` still resolves, as an alias, so existing notes keep
working. The file is `distros/starter/starter.toml`. Configs that still
extend `distros/herdr/herdr.toml` keep loading through a compatibility
stub at that path.

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
