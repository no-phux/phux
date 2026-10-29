**Phux Rune 0.1.0-alpha.3**: the macOS app now opens like any other.

The app is Developer ID signed with the hardened runtime, notarized by Apple
and stapled, through the same signing path as the GPUI desktop, so Gatekeeper
accepts it from Finder, including offline. Earlier alphas were ad-hoc signed
and macOS refused to open them. Nothing else changed since 0.1.0-alpha.2:

- **Agent-aware sidebar**: each terminal's agent and state, running commands,
  failures and bells; sessions count what needs you, and ⌘J jumps there.
- **Command palette** (⌘K): fuzzy search over terminals, sessions and actions.
- **Splits** from the session's server-stored arrangement, shared with the
  terminal UI: ⌘D / ⌘⇧D, ⌘⌥ arrows, ⌘⇧↩ zoom, draggable dividers.
- **Mouse reporting**, alternate scroll, input methods, cursor blink, ⌘W close
  with a guard, paste protection, foldable sidebar, window title.

Alpha: one window, no settings or images yet. It needs the phux CLI and never
installs or upgrades it. Verify an archive with its `.sha256` file. See
[clients/rune/README.md](https://github.com/no-phux/phux/blob/main/clients/rune/README.md).
