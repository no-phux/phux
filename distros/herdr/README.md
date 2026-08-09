---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-08-08
---

# herdr — the phux plugin-bundle layer

**TL;DR.** herdr used to be phux's curated starter distro. Its opinions
are now the **shipped defaults**, so a naked `phux` already looks and
behaves the way installing herdr used to make it behave. What remains
here is the one thing an embedded default cannot express: the demo plugin
set, wired via `[[plugins-append]]`. Install with
`phux config init --distro herdr`.

---

## What happened to the distro

Everything herdr used to add — the 400 ms which-key delay, `Space` for
the command palette, the `|` / `-` split aliases, `Tab` for the next
window, `${cwd-basename}` session naming, the padded blue tab strip, the
tokyonight chrome palette — moved one layer down into
`crates/phux-config/src/default.toml` and
`crates/phux-client/src/render/theme.rs`. Nothing was dropped. It is
simply not a distro's job to hold the settings that everyone should have:
a starter distribution exists to offer a *choice*, and there was no
choice being offered here, only a good default sitting behind an opt-in.

`crates/phux-config/tests/herdr_distro.rs` pins that in both directions —
a config with no distro must carry every one of those opinions, and
extending this layer must not change any of them.

## What is left

Plugin wiring, and only that. `default.toml` ships inside the binary via
`include_str!`, so it has no directory of its own for a relative
`manifest` path to resolve against. A layer file on disk does, which is
why the demo plugin set stays a distro concern.

```sh
phux config init --distro herdr
```

That scaffolds `~/.config/phux/config.toml` with one active line:

```toml
extends = ["/absolute/path/to/distros/herdr/herdr.toml"]
```

followed by the fully-commented shipped defaults. Nothing is copied out
of herdr: the layer file stays authoritative, so pulling a newer phux
checkout updates it for every config that extends it.

Already have a config? Add the `extends` line yourself (top-level, any
config file). See docs/CONFIG.md "Layered configs" for the mechanics.

The bundled name `herdr` resolves through, in order: `$PHUX_DISTROS_DIR`,
`$XDG_DATA_HOME/phux/distros` (default `~/.local/share/phux/distros`),
then the repo checkout's `distros/` directory. A path (`--distro
./distros/herdr` or `--distro ./distros/herdr/herdr.toml`) works from
anywhere.

## What you get

- **Plugins, additively.** `[[plugins-append]]` wires
  `examples/plugins/continuum` (workspace autosave/restore) and
  `examples/plugins/agent-tools` (bench helpers) without erasing plugin
  entries from your own config or another layer. Their actions appear in
  the command palette (`prefix Space`) as `plugin:` rows. To drop them,
  assign a plain `plugins = [...]` in your config — replacement wins over
  inherited appends.
- **Agent identity.** `agent-tools` ships integration templates whose
  `[launch]` command runs `claude`/`codex`/`gemini` through
  `scripts/phux-agent-wrap.sh`. The wrapper writes a `phux.agent/v1` L3
  record (ADR-0040) — a first-class name + kind that the sidebar's
  `agents` section and `phux agent list` prefer over the OSC-title
  substring heuristic that false-positives on titles like
  `vim CLAUDE.md` — and clears it on exit, pinned to the wrapper's own
  pane. Because this layer enables `agent-tools`, `phux launch
  claude-code` (or `codex` / `gemini-cli`) opens a self-identifying pane
  end-to-end: the launch executor (ADR-0042) runs the template `[launch]`
  command and the server injects `PHUX_TERMINAL_ID` so the wrapper
  self-targets with no alias. `phux launch --list` shows the bundled
  integrations. See `examples/plugins/agent-tools/README.md` section
  "Automatic agent identity" for the manual-activation fallback, the
  required pane-targeting, and why live working/blocked state still needs
  a separate signal feed.

## Overriding

Your `config.toml` merges on top of the layer, key by key:

```toml
extends = ["/absolute/path/to/distros/herdr/herdr.toml"]

[keybindings]
which-key-delay-ms = 800        # slow the popup back down

[theme]
accent = "magenta"              # replace one slot, keep the rest
```

`phux config show` prints the effective result of the whole stack, and
`phux config show --layers` says which layer set each key.

## Layout notes

Relative `manifest` paths in a layer resolve against the layer file's
own directory, which is what lets this layer reference the sibling
`examples/plugins/` tree. If you copy `distros/herdr/` out of a phux
checkout, copy those two plugin directories too and update the
`[[plugins-append]]` paths in `herdr.toml`.
