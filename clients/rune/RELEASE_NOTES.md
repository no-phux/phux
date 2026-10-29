The second alpha of **Phux Rune**, a phux desktop client built on
[Rune](https://github.com/anomalyco/rune), beside the GPUI desktop as a
candidate replacement.

- **Agent-aware sidebar**: every terminal shows its agent and state (working,
  needs you, done), running commands, failures and bells; sessions count what
  needs you, and ⌘J jumps there.
- **Command palette** (⌘K): fuzzy search over terminals, sessions and actions.
- **Splits** from the session's server-stored arrangement, shared with the
  terminal UI: ⌘D / ⌘⇧D, ⌘⌥ arrows, ⌘⇧↩ zoom, draggable dividers.
- **Mouse reporting** for vim, htop and friends; alternate scroll in less.
- **Input methods** (dead keys, CJK, emoji), cursor blink, ⌘W close with a
  guard, paste protection, foldable sidebar, window title.

Alpha: one window, no settings or images yet. The macOS app is ad-hoc signed
and not notarized (Control-click → Open the first time). It needs the phux
CLI and never installs or upgrades it.

Verify an archive with its `.sha256` file. See
[clients/rune/README.md](https://github.com/no-phux/phux/blob/main/clients/rune/README.md).
